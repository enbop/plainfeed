//! Orchestration for staged Plainfeed synchronization.

use std::path::PathBuf;

use plainfeed_core::Store;
use plainfeed_sync_core::{
    ConflictReport, activate_staged_snapshot_with_finalize, audit_remote_changes,
};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Debug)]
pub struct ActivationRequest {
    pub repository_root: PathBuf,
    pub branch: String,
    pub expected_base: String,
    pub remote_tip: String,
    pub trusted_state_tree: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationOutcome {
    pub remote_tip: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Git snapshot operation failed: {0}")]
    Git(#[from] plainfeed_git::Error),
    #[error("remote commit has no state tree")]
    MissingStateTree,
    #[error("remote ownership audit failed: {0}")]
    Audit(#[from] plainfeed_sync_core::AuditError),
    #[error("snapshot activation failed: {0}")]
    Activation(#[from] plainfeed_sync_core::Error),
}

pub fn activate_fetched_snapshot(request: ActivationRequest) -> Result<ActivationOutcome, Error> {
    let changed_paths = plainfeed_git::changed_paths(
        &request.repository_root,
        Some(&request.expected_base),
        &request.remote_tip,
    )?;
    let remote_state_tree = plainfeed_git::commit_root_entry_oid(
        &request.repository_root,
        &request.remote_tip,
        "state",
    )?
    .ok_or(Error::MissingStateTree)?;
    audit_remote_changes(
        changed_paths.iter().map(String::as_str),
        Some(&remote_state_tree),
        Some(&request.trusted_state_tree),
    )?;

    let staging = request
        .repository_root
        .join(".plainfeed/staging")
        .join(&request.remote_tip);
    plainfeed_git::export_remote_snapshot(&request.repository_root, &request.remote_tip, &staging)?;
    let result = activate_staged_snapshot_with_finalize(
        &request.repository_root,
        &staging,
        |snapshot| Store::open(snapshot).validate(),
        || {
            plainfeed_git::finalize_fast_forward_checkout(
                &request.repository_root,
                &request.branch,
                Some(&request.expected_base),
                &request.remote_tip,
            )
        },
    );
    if let Err(error) = result {
        let mut report = ConflictReport::new(
            error.to_string(),
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned()),
        );
        report.paths = changed_paths.clone();
        report.local_base = Some(request.expected_base.clone());
        report.remote_tip = Some(request.remote_tip.clone());
        let _ = report.write_to(&request.repository_root);
        return Err(Error::Activation(error));
    }

    Ok(ActivationOutcome {
        remote_tip: request.remote_tip,
        changed_paths,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    use super::{ActivationRequest, activate_fetched_snapshot};

    const OLD_ENTRY: &str = r#"+++
format = "plainfeed.entry/v1"
id = "old-entry"
title = "Old entry"
published = "2026-07-17T00:00:00Z"
summary = "Old"
channels = ["technology"]
source = { name = "Test", url = "https://example.com/old" }
+++

Old body.
"#;

    const NEW_ENTRY: &str = r#"+++
format = "plainfeed.entry/v1"
id = "new-entry"
title = "New entry"
published = "2026-07-17T01:00:00Z"
summary = "New"
channels = ["technology"]
source = { name = "Test", url = "https://example.com/new" }
+++

New body.
"#;

    const CHANNELS: &str = r#"format = "plainfeed.channels/v1"

[[channels]]
id = "technology"
label = "Technology"
"#;

    const STATE: &str = r#"format = "plainfeed.state/v1"
entry_id = "old-entry"
favorite = true
"#;

    #[test]
    fn activates_a_valid_fetched_snapshot_without_touching_reader_state() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("data");
        let repository = gix::init(&root).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let old_content = repository.write_blob(OLD_ENTRY.as_bytes()).unwrap();
        let config = repository.write_blob(CHANNELS.as_bytes()).unwrap();
        let state = repository.write_blob(STATE.as_bytes()).unwrap();
        let mut old_editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        old_editor
            .upsert("content/old-entry.md", EntryKind::Blob, old_content)
            .unwrap();
        old_editor
            .upsert("config/channels.toml", EntryKind::Blob, config)
            .unwrap();
        old_editor
            .upsert("state/entries/old-entry.toml", EntryKind::Blob, state)
            .unwrap();
        let old_tree = old_editor.write().unwrap();
        let old_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/main",
                "old snapshot",
                old_tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        repository
            .index_from_tree(&old_tree)
            .unwrap()
            .write(Default::default())
            .unwrap();

        let new_content = repository.write_blob(NEW_ENTRY.as_bytes()).unwrap();
        let mut new_editor = repository.edit_tree(old_tree).unwrap();
        new_editor.remove("content/old-entry.md").unwrap();
        new_editor
            .upsert("content/new-entry.md", EntryKind::Blob, new_content)
            .unwrap();
        let new_tree = new_editor.write().unwrap();
        let new_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/remotes/origin/main",
                "new snapshot",
                new_tree,
                [old_commit.detach()],
            )
            .unwrap();

        fs::create_dir_all(root.join("content")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir_all(root.join("state/entries")).unwrap();
        fs::write(root.join("content/old-entry.md"), OLD_ENTRY).unwrap();
        fs::write(root.join("config/channels.toml"), CHANNELS).unwrap();
        fs::write(root.join("state/entries/old-entry.toml"), STATE).unwrap();
        let trusted_state_tree = plainfeed_git::commit_root_entry_oid(
            repository.path(),
            &old_commit.to_string(),
            "state",
        )
        .unwrap()
        .unwrap();

        activate_fetched_snapshot(ActivationRequest {
            repository_root: root.clone(),
            branch: "main".to_owned(),
            expected_base: old_commit.to_string(),
            remote_tip: new_commit.to_string(),
            trusted_state_tree,
        })
        .unwrap();

        assert!(!root.join("content/old-entry.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("content/new-entry.md")).unwrap(),
            NEW_ENTRY
        );
        assert_eq!(
            fs::read_to_string(root.join("state/entries/old-entry.toml")).unwrap(),
            STATE
        );
        assert_eq!(
            repository
                .find_reference("refs/heads/main")
                .unwrap()
                .into_fully_peeled_id()
                .unwrap()
                .to_string(),
            new_commit.to_string()
        );
    }

    #[test]
    fn invalid_remote_content_keeps_live_snapshot_and_writes_conflict_report() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("data");
        let repository = gix::init(&root).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let old_content = repository.write_blob(OLD_ENTRY.as_bytes()).unwrap();
        let state = repository.write_blob(STATE.as_bytes()).unwrap();
        let mut old_editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        old_editor
            .upsert("content/old-entry.md", EntryKind::Blob, old_content)
            .unwrap();
        old_editor
            .upsert("state/entries/old-entry.toml", EntryKind::Blob, state)
            .unwrap();
        let old_tree = old_editor.write().unwrap();
        let old_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/main",
                "old snapshot",
                old_tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        let invalid = repository
            .write_blob(b"not Plainfeed front matter\n")
            .unwrap();
        let mut new_editor = repository.edit_tree(old_tree).unwrap();
        new_editor
            .upsert("content/old-entry.md", EntryKind::Blob, invalid)
            .unwrap();
        let new_tree = new_editor.write().unwrap();
        let new_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/remotes/origin/main",
                "invalid snapshot",
                new_tree,
                [old_commit.detach()],
            )
            .unwrap();
        fs::create_dir_all(root.join("content")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir_all(root.join("state/entries")).unwrap();
        fs::write(root.join("content/old-entry.md"), OLD_ENTRY).unwrap();
        fs::write(root.join("state/entries/old-entry.toml"), STATE).unwrap();
        let trusted_state_tree = plainfeed_git::commit_root_entry_oid(
            repository.path(),
            &old_commit.to_string(),
            "state",
        )
        .unwrap()
        .unwrap();

        let result = activate_fetched_snapshot(ActivationRequest {
            repository_root: root.clone(),
            branch: "main".to_owned(),
            expected_base: old_commit.to_string(),
            remote_tip: new_commit.to_string(),
            trusted_state_tree,
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(root.join("content/old-entry.md")).unwrap(),
            OLD_ENTRY
        );
        let conflict = fs::read_to_string(root.join(".plainfeed/conflict.toml")).unwrap();
        assert!(conflict.contains("content/old-entry.md"));
        assert!(conflict.contains(&new_commit.to_string()));
        assert_eq!(
            repository
                .find_reference("refs/heads/main")
                .unwrap()
                .into_fully_peeled_id()
                .unwrap()
                .to_string(),
            old_commit.to_string()
        );
    }
}
