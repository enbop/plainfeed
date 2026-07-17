use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

use gix::{bstr::ByteSlice, objs::tree::EntryKind};

use crate::{Error, FetchLimits, Remote, http::Transport};

#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub repository: PathBuf,
    pub remote: Remote,
    pub branch: String,
    pub limits: FetchLimits,
}

impl FetchRequest {
    pub fn main(repository: impl Into<PathBuf>, remote: Remote) -> Self {
        Self {
            repository: repository.into(),
            remote,
            branch: "main".to_owned(),
            limits: FetchLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchOutcome {
    pub remote_tip: String,
    pub state_tree: Option<String>,
    pub remote_refs: usize,
    pub repository_bytes: u64,
}

pub async fn fetch(request: FetchRequest) -> Result<FetchOutcome, Error> {
    validate_branch(&request.branch)?;
    let mut repository = match gix::open(&request.repository) {
        Ok(repository) => repository,
        Err(_) => gix::init(&request.repository).map_err(git_error)?,
    };
    request.limits.check_repository(repository.git_dir())?;
    repository
        .committer_or_set_generic_fallback()
        .map_err(git_error)?;

    let source_ref = format!("refs/heads/{}", request.branch);
    let destination_ref = format!("refs/remotes/origin/{}", request.branch);
    let refspec = format!("{source_ref}:{destination_ref}");
    let remote = repository
        .remote_at(request.remote.url())
        .map_err(git_error)?
        .with_refspecs(Some(refspec.as_str()), gix::remote::Direction::Fetch)
        .map_err(git_error)?;
    let transport = Transport::new(request.remote, request.limits)?;
    let connection = remote.to_connection_with_transport(transport);
    let prepared = connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .await
        .map_err(git_error)?;
    let outcome = prepared
        .receive(gix::progress::Discard, &AtomicBool::new(false))
        .await
        .map_err(git_error)?;

    let reopened = gix::open(repository.git_dir()).map_err(git_error)?;
    let tip = reopened
        .find_reference(destination_ref.as_str())
        .map_err(git_error)?
        .into_fully_peeled_id()
        .map_err(git_error)?;
    let commit = reopened.find_commit(tip).map_err(git_error)?;
    let tree = commit.tree().map_err(git_error)?;
    let state_tree = tree
        .find_entry("state")
        .map(|entry| entry.object_id().to_string());
    let repository_bytes = request.limits.check_repository(reopened.git_dir())?;

    Ok(FetchOutcome {
        remote_tip: tip.to_string(),
        state_tree,
        remote_refs: outcome.ref_map.remote_refs.len(),
        repository_bytes,
    })
}

pub fn changed_paths(
    repository_path: impl AsRef<Path>,
    previous_commit: Option<&str>,
    remote_commit: &str,
) -> Result<Vec<String>, Error> {
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let mut previous = BTreeMap::new();
    if let Some(previous_commit) = previous_commit {
        let tree = find_commit_tree(&repository, previous_commit)?;
        collect_tree_entries(&tree, "", &mut previous)?;
    }
    let tree = find_commit_tree(&repository, remote_commit)?;
    let mut remote = BTreeMap::new();
    collect_tree_entries(&tree, "", &mut remote)?;

    let all_paths: BTreeSet<_> = previous.keys().chain(remote.keys()).cloned().collect();
    Ok(all_paths
        .into_iter()
        .filter(|path| previous.get(path) != remote.get(path))
        .collect())
}

fn find_commit_tree<'repo>(
    repository: &'repo gix::Repository,
    oid: &str,
) -> Result<gix::Tree<'repo>, Error> {
    let oid = gix::hash::ObjectId::from_hex(oid.as_bytes()).map_err(git_error)?;
    repository
        .find_commit(oid)
        .map_err(git_error)?
        .tree()
        .map_err(git_error)
}

fn collect_tree_entries(
    tree: &gix::Tree<'_>,
    prefix: &str,
    entries: &mut BTreeMap<String, (gix::objs::tree::EntryMode, gix::hash::ObjectId)>,
) -> Result<(), Error> {
    for entry in tree.iter() {
        let entry = entry.map_err(git_error)?;
        let name = entry
            .filename()
            .to_str()
            .map_err(|error| Error::Git(error.to_string()))?;
        if matches!(name, "." | "..") || name.contains(['/', '\0']) {
            return Err(Error::Git(format!(
                "unsafe Git tree path component {name:?}"
            )));
        }
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.kind() == EntryKind::Tree {
            let child = entry.object().map_err(git_error)?.into_tree();
            collect_tree_entries(&child, &path, entries)?;
        } else {
            entries.insert(path, (entry.mode(), entry.object_id()));
        }
    }
    Ok(())
}

pub fn export_remote_snapshot(
    repository_path: impl AsRef<Path>,
    commit_oid: &str,
    destination: impl AsRef<Path>,
) -> Result<(), Error> {
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let oid = gix::hash::ObjectId::from_hex(commit_oid.as_bytes()).map_err(git_error)?;
    let commit = repository.find_commit(oid).map_err(git_error)?;
    let root_tree = commit.tree().map_err(git_error)?;
    let destination = destination.as_ref();
    if destination.is_dir() {
        return Ok(());
    }
    let parent = destination.parent().ok_or_else(|| {
        Error::Git(format!(
            "snapshot destination has no parent: {}",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|source| Error::Io {
        path: parent.to_owned(),
        source,
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".snapshot-{nonce:032x}.tmp"));
    fs::create_dir(&temporary).map_err(|source| Error::Io {
        path: temporary.clone(),
        source,
    })?;

    let result = (|| {
        for area in ["content", "config"] {
            let output = temporary.join(area);
            fs::create_dir(&output).map_err(|source| Error::Io {
                path: output.clone(),
                source,
            })?;
            if let Some(entry) = root_tree.find_entry(area) {
                if entry.kind() != EntryKind::Tree {
                    return Err(Error::Git(format!("remote {area} path is not a tree")));
                }
                let tree = entry.object().map_err(git_error)?.into_tree();
                export_tree(&tree, &output)?;
            }
        }
        fs::rename(&temporary, destination).map_err(|source| Error::Io {
            path: destination.to_owned(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

pub fn commit_root_entry_oid(
    repository_path: impl AsRef<Path>,
    commit_oid: &str,
    entry_name: &str,
) -> Result<Option<String>, Error> {
    if entry_name.is_empty() || entry_name.contains(['/', '\0']) || matches!(entry_name, "." | "..")
    {
        return Err(Error::Git(format!(
            "invalid root tree entry {entry_name:?}"
        )));
    }
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let oid = gix::hash::ObjectId::from_hex(commit_oid.as_bytes()).map_err(git_error)?;
    let tree = repository
        .find_commit(oid)
        .map_err(git_error)?
        .tree()
        .map_err(git_error)?;
    Ok(tree
        .find_entry(entry_name)
        .map(|entry| entry.object_id().to_string()))
}

pub fn reference_oid(
    repository_path: impl AsRef<Path>,
    reference_name: &str,
) -> Result<String, Error> {
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    Ok(repository
        .find_reference(reference_name)
        .map_err(git_error)?
        .into_fully_peeled_id()
        .map_err(git_error)?
        .to_string())
}

fn export_tree(tree: &gix::Tree<'_>, destination: &Path) -> Result<(), Error> {
    for entry in tree.iter() {
        let entry = entry.map_err(git_error)?;
        let name = entry
            .filename()
            .to_str()
            .map_err(|error| Error::Git(error.to_string()))?;
        if matches!(name, "." | ".." | ".git") || name.contains(['/', '\0']) {
            return Err(Error::Git(format!(
                "unsafe path component in remote tree: {name:?}"
            )));
        }
        let output = destination.join(name);
        match entry.kind() {
            EntryKind::Tree => {
                fs::create_dir(&output).map_err(|source| Error::Io {
                    path: output.clone(),
                    source,
                })?;
                let child = entry.object().map_err(git_error)?.into_tree();
                export_tree(&child, &output)?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => {
                let blob = entry.object().map_err(git_error)?.into_blob();
                fs::write(&output, &blob.data).map_err(|source| Error::Io {
                    path: output,
                    source,
                })?;
            }
            EntryKind::Link | EntryKind::Commit => {
                return Err(Error::Git(format!(
                    "unsupported remote tree entry kind at {}",
                    output.display()
                )));
            }
        }
    }
    Ok(())
}

pub fn finalize_fast_forward_checkout(
    repository_path: impl AsRef<Path>,
    branch: &str,
    expected_previous: Option<&str>,
    remote_tip: &str,
) -> Result<(), Error> {
    validate_branch(branch)?;
    let mut repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    repository
        .committer_or_set_generic_fallback()
        .map_err(git_error)?;
    let remote_oid = gix::hash::ObjectId::from_hex(remote_tip.as_bytes()).map_err(git_error)?;
    let remote_commit = repository.find_commit(remote_oid).map_err(git_error)?;
    let expected_oid = expected_previous
        .map(|value| gix::hash::ObjectId::from_hex(value.as_bytes()).map_err(git_error))
        .transpose()?;

    if let Some(expected_oid) = expected_oid {
        let mut found = false;
        for commit in remote_commit.ancestors().all().map_err(git_error)? {
            if commit.map_err(git_error)?.id == expected_oid {
                found = true;
                break;
            }
        }
        if !found {
            return Err(Error::NonFastForward {
                base: expected_oid.to_string(),
                remote: remote_oid.to_string(),
            });
        }
    }

    let index_path = repository.index_path();
    let previous_index = match fs::read(&index_path) {
        Ok(bytes) => Some(bytes),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(Error::Io {
                path: index_path.to_owned(),
                source,
            });
        }
    };
    repository
        .index_from_tree(&remote_commit.tree_id().map_err(git_error)?)
        .map_err(git_error)?
        .write(Default::default())
        .map_err(git_error)?;

    let constraint = match expected_oid {
        Some(expected_oid) => gix::refs::transaction::PreviousValue::MustExistAndMatch(
            gix::refs::Target::Object(expected_oid),
        ),
        None => gix::refs::transaction::PreviousValue::MustNotExist,
    };
    let reference_name = format!("refs/heads/{branch}");
    let reference_name =
        gix::refs::FullName::try_from(reference_name.as_str()).map_err(git_error)?;
    let committer = repository
        .committer()
        .transpose()
        .map_err(git_error)?
        .ok_or_else(|| Error::Git("fallback committer is unavailable".to_owned()))?;
    let edit = gix::refs::transaction::RefEdit {
        change: gix::refs::transaction::Change::Update {
            log: gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::AndReference,
                force_create_reflog: false,
                message: "plainfeed: activate synchronized snapshot".into(),
            },
            expected: constraint,
            new: gix::refs::Target::Object(remote_oid),
        },
        name: reference_name,
        deref: false,
    };
    if let Err(error) = repository.edit_references_as(Some(edit), Some(committer)) {
        restore_index(&index_path, previous_index.as_deref())?;
        return Err(git_error(error));
    }
    Ok(())
}

fn restore_index(path: &Path, bytes: Option<&[u8]>) -> Result<(), Error> {
    match bytes {
        Some(bytes) => {
            let temporary = path.with_extension("plainfeed-restore.tmp");
            fs::write(&temporary, bytes).map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
            fs::rename(&temporary, path).map_err(|source| Error::Io {
                path: path.to_owned(),
                source,
            })
        }
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io {
                path: path.to_owned(),
                source,
            }),
        },
    }
}

fn validate_branch(branch: &str) -> Result<(), Error> {
    let forbidden = ['~', '^', ':', '?', '*', '[', '\\'];
    if branch.is_empty()
        || branch.starts_with('-')
        || branch.contains("..")
        || branch
            .chars()
            .any(|character| forbidden.contains(&character))
        || branch
            .split('/')
            .any(|segment| segment.is_empty() || segment.ends_with('.'))
    {
        return Err(Error::Git(format!("invalid branch name {branch:?}")));
    }
    Ok(())
}

fn git_error(error: impl std::fmt::Display + std::fmt::Debug) -> Error {
    Error::Git(format!("{error:#?}"))
}

#[cfg(test)]
mod tests {
    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    use super::{
        changed_paths, export_remote_snapshot, finalize_fast_forward_checkout, validate_branch,
    };

    #[test]
    fn accepts_normal_branches_and_rejects_refspec_injection() {
        assert!(validate_branch("main").is_ok());
        assert!(validate_branch("projects/plainfeed").is_ok());
        for invalid in ["", "../main", "main..old", "bad:main", "topic/", "-main"] {
            assert!(validate_branch(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn exports_only_content_and_config_from_a_fetched_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let content = repository.write_blob(b"entry body\n").unwrap();
        let config = repository.write_blob(b"channel config\n").unwrap();
        let state = repository.write_blob(b"must not export\n").unwrap();
        let mut editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        editor
            .upsert("content/entry.md", EntryKind::Blob, content)
            .unwrap();
        editor
            .upsert("config/channels.toml", EntryKind::Blob, config)
            .unwrap();
        editor
            .upsert("state/entries/private.toml", EntryKind::Blob, state)
            .unwrap();
        let tree = editor.write().unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let commit = repository
            .commit_as(
                identity,
                identity,
                "HEAD",
                "snapshot fixture",
                tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        let staging = temporary.path().join("staging/commit");

        export_remote_snapshot(repository.path(), &commit.to_string(), &staging).unwrap();

        assert_eq!(
            std::fs::read_to_string(staging.join("content/entry.md")).unwrap(),
            "entry body\n"
        );
        assert_eq!(
            std::fs::read_to_string(staging.join("config/channels.toml")).unwrap(),
            "channel config\n"
        );
        assert!(!staging.join("state").exists());
    }

    #[test]
    fn finalizes_index_and_branch_with_compare_and_swap() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let old_blob = repository.write_blob(b"old\n").unwrap();
        let mut old_editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        let old_tree = old_editor
            .upsert("content/item.md", EntryKind::Blob, old_blob)
            .unwrap()
            .write()
            .unwrap();
        let old_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/main",
                "old",
                old_tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        repository
            .index_from_tree(&old_tree)
            .unwrap()
            .write(Default::default())
            .unwrap();

        let new_blob = repository.write_blob(b"new\n").unwrap();
        let mut new_editor = repository.edit_tree(old_tree).unwrap();
        let new_tree = new_editor
            .upsert("content/item.md", EntryKind::Blob, new_blob)
            .unwrap()
            .write()
            .unwrap();
        let new_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/remotes/origin/main",
                "new",
                new_tree,
                [old_commit.detach()],
            )
            .unwrap();

        assert_eq!(
            changed_paths(
                repository.path(),
                Some(&old_commit.to_string()),
                &new_commit.to_string(),
            )
            .unwrap(),
            ["content/item.md"]
        );

        finalize_fast_forward_checkout(
            repository.path(),
            "main",
            Some(&old_commit.to_string()),
            &new_commit.to_string(),
        )
        .unwrap();

        let reopened = gix::open(repository.path()).unwrap();
        assert_eq!(
            reopened
                .find_reference("refs/heads/main")
                .unwrap()
                .into_fully_peeled_id()
                .unwrap()
                .to_string(),
            new_commit.to_string()
        );
        assert!(
            reopened
                .open_index()
                .unwrap()
                .entries()
                .iter()
                .any(|entry| entry.id == new_blob.detach())
        );

        let third_blob = repository.write_blob(b"third\n").unwrap();
        let mut third_editor = repository.edit_tree(new_tree).unwrap();
        let third_tree = third_editor
            .upsert("content/item.md", EntryKind::Blob, third_blob)
            .unwrap()
            .write()
            .unwrap();
        let third_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/remotes/origin/main",
                "third",
                third_tree,
                [new_commit.detach()],
            )
            .unwrap();

        assert!(
            finalize_fast_forward_checkout(
                repository.path(),
                "main",
                Some(&old_commit.to_string()),
                &third_commit.to_string(),
            )
            .is_err()
        );
        let reopened = gix::open(repository.path()).unwrap();
        assert_eq!(
            reopened
                .find_reference("refs/heads/main")
                .unwrap()
                .into_fully_peeled_id()
                .unwrap()
                .to_string(),
            new_commit.to_string()
        );
        let restored_index = reopened.open_index().unwrap();
        assert!(
            restored_index
                .entries()
                .iter()
                .any(|entry| entry.id == new_blob.detach())
        );
        assert!(
            !restored_index
                .entries()
                .iter()
                .any(|entry| entry.id == third_blob.detach())
        );
    }
}
