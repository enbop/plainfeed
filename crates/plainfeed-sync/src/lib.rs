//! Orchestration for staged Plainfeed synchronization.

use std::{fs, path::PathBuf};

use plainfeed_core::Store;
use plainfeed_sync_core::{
    ConflictReport, DirtyJournal, PendingPush, SyncState, activate_staged_snapshot_with_finalize,
    audit_plainfeed_changes, audit_remote_changes,
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
    #[error("state publication lost the remote race three times")]
    PushRetryExhausted,
    #[error("reader state is invalid: {0}")]
    InvalidState(#[from] plainfeed_core::Error),
    #[error("local changes are outside Plainfeed-owned state paths: {paths:?}")]
    LocalOwnership { paths: Vec<String> },
    #[error("synchronization is blocked by an active conflict: {reason}")]
    ConflictActive { reason: String },
    #[error("cannot initialize Git because the data directory contains {path}")]
    InitializationTargetNotEmpty { path: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    NoDirtyState,
    AlreadyPublished,
    Pushed(plainfeed_git::PushOutcome),
}

pub fn recover_local_transition(repository_root: impl Into<PathBuf>) -> Result<bool, Error> {
    let repository_root = repository_root.into();
    let lock = repository_root.join(".plainfeed/update.lock");
    if !lock.is_dir() {
        return Ok(false);
    }
    let local_tip = plainfeed_git::reference_oid(&repository_root, "refs/heads/main")?;
    let changed = plainfeed_git::worktree_changed_paths(
        &repository_root,
        &local_tip,
        &["content", "config"],
    )?;
    let backup_root = repository_root.join(".plainfeed/backup");
    if !changed.is_empty() {
        let mut backups = match fs::read_dir(&backup_root) {
            Ok(entries) => entries.collect::<Result<Vec<_>, _>>().map_err(|source| {
                plainfeed_sync_core::Error::Io {
                    path: backup_root.clone(),
                    source,
                }
            })?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(plainfeed_sync_core::Error::Io {
                    path: backup_root,
                    source,
                }
                .into());
            }
        };
        backups.sort_by_key(|entry| entry.file_name());
        let backup = backups.last().map(|entry| entry.path()).ok_or_else(|| {
            plainfeed_sync_core::Error::Validation(
                "interrupted activation has no rollback backup".to_owned(),
            )
        })?;
        for area in ["content", "config"] {
            let previous = backup.join(area);
            if !previous.exists() {
                continue;
            }
            let live = repository_root.join(area);
            match fs::remove_dir_all(&live) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(plainfeed_sync_core::Error::Io { path: live, source }.into());
                }
            }
            fs::rename(&previous, &live)
                .map_err(|source| plainfeed_sync_core::Error::Io { path: live, source })?;
        }
        Store::open(&repository_root).validate()?;
        let remaining = plainfeed_git::worktree_changed_paths(
            &repository_root,
            &local_tip,
            &["content", "config"],
        )?;
        if !remaining.is_empty() {
            return Err(Error::LocalOwnership { paths: remaining });
        }
    }

    let mut state = SyncState::read_from(&repository_root)?
        .unwrap_or_else(|| SyncState::new("origin", "refs/heads/main"));
    state.last_remote_oid = Some(local_tip.clone());
    state.last_state_tree_oid =
        plainfeed_git::commit_root_entry_oid(&repository_root, &local_tip, "state")?;
    state.last_error = None;
    state.write_to(&repository_root)?;

    match fs::remove_dir_all(&backup_root) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(plainfeed_sync_core::Error::Io {
                path: backup_root,
                source,
            }
            .into());
        }
    }
    fs::remove_dir(&lock)
        .map_err(|source| plainfeed_sync_core::Error::Io { path: lock, source })?;
    Ok(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncCommand {
    Tick,
    Force,
    Status,
}

pub fn pull_is_due(command: SyncCommand, last_pull_at: Option<&str>, now: OffsetDateTime) -> bool {
    match command {
        SyncCommand::Force => true,
        SyncCommand::Status => false,
        SyncCommand::Tick => {
            let Some(last_pull_at) = last_pull_at else {
                return true;
            };
            let Ok(last_pull_at) = OffsetDateTime::parse(last_pull_at, &Rfc3339) else {
                return true;
            };
            now - last_pull_at >= time::Duration::minutes(5)
        }
    }
}

pub fn state_publication_is_due(
    command: SyncCommand,
    marker_names: &[String],
    now: OffsetDateTime,
) -> bool {
    if marker_names.is_empty() || command == SyncCommand::Status {
        return false;
    }
    if command == SyncCommand::Force {
        return true;
    }
    let mut timestamps = marker_names.iter().filter_map(|name| {
        name.split_once('-')
            .and_then(|(timestamp, _)| u128::from_str_radix(timestamp, 16).ok())
            .and_then(|timestamp| i128::try_from(timestamp).ok())
    });
    let Some(first) = timestamps.next() else {
        return true;
    };
    let (oldest, newest) = timestamps.fold((first, first), |(oldest, newest), timestamp| {
        (oldest.min(timestamp), newest.max(timestamp))
    });
    let now = now.unix_timestamp_nanos();
    now.saturating_sub(newest) >= 30_000_000_000 || now.saturating_sub(oldest) >= 300_000_000_000
}

pub async fn run_pull_cycle(
    command: SyncCommand,
    repository_root: impl Into<PathBuf>,
    remote: plainfeed_git::Remote,
    now: OffsetDateTime,
) -> Result<bool, Error> {
    let repository_root = repository_root.into();
    if initialize_repository_if_needed(&repository_root, remote.clone(), now).await? {
        return Ok(true);
    }
    ensure_no_active_conflict(&repository_root)?;
    preflight_repository_contract(&repository_root, now)?;
    let mut state = SyncState::read_from(&repository_root)?
        .unwrap_or_else(|| SyncState::new("origin", "refs/heads/main"));
    if !pull_is_due(command, state.last_pull_at.as_deref(), now) {
        return Ok(false);
    }

    let local_tip = plainfeed_git::reference_oid(&repository_root, "refs/heads/main")?;
    preflight_local_ownership(&repository_root, &local_tip, now)?;
    let trusted_state_tree = trusted_state_tree(
        &repository_root,
        &local_tip,
        state.last_state_tree_oid.as_deref(),
        now,
    )?;
    let successful_remote_url = remote.url().to_owned();
    let fetched =
        match plainfeed_git::fetch(plainfeed_git::FetchRequest::main(&repository_root, remote))
            .await
        {
            Ok(fetched) => fetched,
            Err(error) => {
                state.last_error = Some(error.to_string());
                let _ = state.write_to(&repository_root);
                if error.is_repository_contract_violation() {
                    write_conflict_report(
                        &repository_root,
                        error.to_string(),
                        Vec::new(),
                        Some(local_tip.clone()),
                        None,
                        now,
                    )?;
                }
                return Err(error.into());
            }
        };
    preflight_remote_history(&repository_root, &local_tip, &fetched.remote_tip, now)?;
    if fetched.remote_tip != local_tip {
        activate_fetched_snapshot(ActivationRequest {
            repository_root: repository_root.clone(),
            branch: "main".to_owned(),
            expected_base: local_tip,
            remote_tip: fetched.remote_tip.clone(),
            trusted_state_tree,
        })?;
        state = SyncState::read_from(&repository_root)?.unwrap_or(state);
    }

    state.remote_url = Some(successful_remote_url);
    state.last_remote_oid = Some(fetched.remote_tip);
    state.last_state_tree_oid = fetched.state_tree;
    state.last_pull_at = Some(
        now.format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned()),
    );
    state.last_error = None;
    state.write_to(&repository_root)?;
    Ok(true)
}

async fn initialize_repository_if_needed(
    repository_root: &std::path::Path,
    remote: plainfeed_git::Remote,
    now: OffsetDateTime,
) -> Result<bool, Error> {
    if plainfeed_git::reference_exists(repository_root, "refs/heads/main")? {
        return Ok(false);
    }
    ensure_initialization_target_is_empty(repository_root)?;
    if ConflictReport::read_from(repository_root)?
        .is_some_and(|report| report.local_base.is_none() && report.remote_tip.is_none())
    {
        ConflictReport::clear(repository_root)?;
    }

    let remote_url = remote.url().to_owned();
    let fetched =
        plainfeed_git::fetch(plainfeed_git::FetchRequest::main(repository_root, remote)).await?;
    let paths = plainfeed_git::changed_paths(repository_root, None, &fetched.remote_tip)?;
    let Some(state_tree) = fetched.state_tree else {
        return Err(Error::MissingStateTree);
    };
    audit_remote_changes(
        paths.iter().map(String::as_str),
        Some(&state_tree),
        Some(&state_tree),
    )?;

    let staging = repository_root
        .join(".plainfeed/staging")
        .join(format!("initial-{}", fetched.remote_tip));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|source| plainfeed_sync_core::Error::Io {
            path: staging.clone(),
            source,
        })?;
    }
    plainfeed_git::export_initial_snapshot(repository_root, &fetched.remote_tip, &staging)?;
    Store::open(&staging).validate()?;

    let mut installed = Vec::new();
    let installation = (|| -> Result<(), Error> {
        let entries = fs::read_dir(&staging).map_err(|source| plainfeed_sync_core::Error::Io {
            path: staging.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| plainfeed_sync_core::Error::Io {
                path: staging.clone(),
                source,
            })?;
            let destination = repository_root.join(entry.file_name());
            if destination.exists() {
                return Err(Error::InitializationTargetNotEmpty { path: destination });
            }
            fs::rename(entry.path(), &destination).map_err(|source| {
                plainfeed_sync_core::Error::Io {
                    path: destination.clone(),
                    source,
                }
            })?;
            installed.push(destination);
        }
        plainfeed_git::set_head_branch(repository_root, "main")?;
        plainfeed_git::finalize_fast_forward_checkout(
            repository_root,
            "main",
            None,
            &fetched.remote_tip,
        )?;
        Ok(())
    })();
    if let Err(error) = installation {
        for path in installed.iter().rev() {
            remove_path(path);
        }
        return Err(error);
    }
    let _ = fs::remove_dir_all(&staging);

    let timestamp = now
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    let mut state = SyncState::new("origin", "refs/heads/main");
    state.remote_url = Some(remote_url);
    state.last_remote_oid = Some(fetched.remote_tip);
    state.last_state_tree_oid = Some(state_tree);
    state.last_pull_at = Some(timestamp);
    state.last_error = None;
    state.write_to(repository_root)?;
    Ok(true)
}

fn ensure_initialization_target_is_empty(repository_root: &std::path::Path) -> Result<(), Error> {
    fs::create_dir_all(repository_root).map_err(|source| plainfeed_sync_core::Error::Io {
        path: repository_root.to_owned(),
        source,
    })?;
    for entry in fs::read_dir(repository_root).map_err(|source| plainfeed_sync_core::Error::Io {
        path: repository_root.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| plainfeed_sync_core::Error::Io {
            path: repository_root.to_owned(),
            source,
        })?;
        if matches!(entry.file_name().to_str(), Some(".git" | ".plainfeed")) {
            continue;
        }
        return Err(Error::InitializationTargetNotEmpty { path: entry.path() });
    }
    Ok(())
}

fn remove_path(path: &std::path::Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

pub async fn publish_state(
    repository_root: impl Into<PathBuf>,
    remote: plainfeed_git::Remote,
    now: OffsetDateTime,
) -> Result<PublishOutcome, Error> {
    let repository_root = repository_root.into();
    ensure_no_active_conflict(&repository_root)?;
    preflight_repository_contract(&repository_root, now)?;
    let recovered_push = recover_pending_push(&repository_root, remote.clone(), now).await?;
    let journal = DirtyJournal::new(&repository_root);
    let dirty_snapshot = journal.snapshot()?;
    if dirty_snapshot.is_empty() {
        return Ok(if recovered_push {
            PublishOutcome::AlreadyPublished
        } else {
            PublishOutcome::NoDirtyState
        });
    }
    if let Err(error) = Store::open(&repository_root).validate_state() {
        write_conflict_report(
            &repository_root,
            error.to_string(),
            vec!["state".to_owned()],
            None,
            None,
            now,
        )?;
        return Err(Error::InvalidState(error));
    }
    let remote_url = remote.url().to_owned();
    let mut state = SyncState::read_from(&repository_root)?
        .unwrap_or_else(|| SyncState::new("origin", "refs/heads/main"));
    let timestamp = now
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    let mut last_stale_remote = None;

    for attempt in 0..3 {
        let local_tip = plainfeed_git::reference_oid(&repository_root, "refs/heads/main")?;
        preflight_local_ownership(&repository_root, &local_tip, now)?;
        let trusted_state_tree = trusted_state_tree(
            &repository_root,
            &local_tip,
            state.last_state_tree_oid.as_deref(),
            now,
        )?;
        let fetched = match plainfeed_git::fetch(plainfeed_git::FetchRequest::main(
            &repository_root,
            remote.clone(),
        ))
        .await
        {
            Ok(fetched) => fetched,
            Err(error) => {
                if error.is_repository_contract_violation() {
                    write_conflict_report(
                        &repository_root,
                        error.to_string(),
                        Vec::new(),
                        Some(local_tip.clone()),
                        None,
                        now,
                    )?;
                }
                return Err(error.into());
            }
        };
        preflight_remote_history(&repository_root, &local_tip, &fetched.remote_tip, now)?;
        let base = state
            .last_remote_oid
            .as_deref()
            .unwrap_or(local_tip.as_str());
        let remote_paths =
            plainfeed_git::changed_paths(&repository_root, Some(base), &fetched.remote_tip)?;
        if let Err(error) = audit_remote_changes(
            remote_paths.iter().map(String::as_str),
            fetched.state_tree.as_deref(),
            Some(&trusted_state_tree),
        ) {
            write_conflict_report(
                &repository_root,
                error.to_string(),
                remote_paths,
                Some(local_tip.clone()),
                Some(fetched.remote_tip.clone()),
                now,
            )?;
            return Err(Error::Audit(error));
        }

        if fetched.remote_tip != local_tip {
            activate_fetched_snapshot(ActivationRequest {
                repository_root: repository_root.clone(),
                branch: "main".to_owned(),
                expected_base: local_tip,
                remote_tip: fetched.remote_tip.clone(),
                trusted_state_tree,
            })?;
            state = SyncState::read_from(&repository_root)?.unwrap_or(state);
        }

        let candidate_ref = format!(
            "refs/plainfeed/state-candidate-{:032x}-{attempt}",
            now.unix_timestamp_nanos()
        );
        let candidate = plainfeed_git::create_state_commit(
            &repository_root,
            &fetched.remote_tip,
            repository_root.join("state"),
            &candidate_ref,
            now.unix_timestamp(),
        )?;
        let Some(candidate) = candidate else {
            state.remote_url = Some(remote_url.clone());
            finalize_publication_state(
                &repository_root,
                &mut state,
                &fetched.remote_tip,
                fetched.state_tree,
                &timestamp,
            )?;
            journal.clear_snapshot(&dirty_snapshot)?;
            return Ok(PublishOutcome::AlreadyPublished);
        };
        audit_plainfeed_changes(candidate.changed_paths.iter().map(String::as_str))?;
        let pending = PendingPush::new(
            &fetched.remote_tip,
            &candidate.commit,
            &candidate.state_tree,
            dirty_snapshot.clone(),
            &timestamp,
            &remote_url,
            &candidate_ref,
        );
        pending.write_to(&repository_root)?;
        match plainfeed_git::push_one_commit(
            remote.clone(),
            &repository_root,
            &candidate_ref,
            "refs/heads/main",
            Default::default(),
        )
        .await
        {
            Ok(outcome) => {
                plainfeed_git::finalize_fast_forward_checkout(
                    &repository_root,
                    "main",
                    Some(&fetched.remote_tip),
                    &candidate.commit,
                )?;
                state.remote_url = Some(remote_url.clone());
                finalize_publication_state(
                    &repository_root,
                    &mut state,
                    &candidate.commit,
                    Some(candidate.state_tree),
                    &timestamp,
                )?;
                journal.clear_snapshot(&dirty_snapshot)?;
                plainfeed_git::delete_plainfeed_reference(&repository_root, &candidate_ref)?;
                PendingPush::clear(&repository_root)?;
                return Ok(PublishOutcome::Pushed(outcome));
            }
            Err(plainfeed_git::Error::StaleRemote { parent, remote }) => {
                plainfeed_git::delete_plainfeed_reference(&repository_root, &candidate_ref)?;
                PendingPush::clear(&repository_root)?;
                last_stale_remote = Some((parent, remote));
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let (local_base, remote_tip) = last_stale_remote
        .map(|(parent, remote)| (Some(parent), Some(remote)))
        .unwrap_or_else(|| (state.last_remote_oid.clone(), None));
    write_conflict_report(
        &repository_root,
        "state publication lost the remote race three times",
        Vec::new(),
        local_base,
        remote_tip,
        now,
    )?;
    Err(Error::PushRetryExhausted)
}

async fn recover_pending_push(
    repository_root: &std::path::Path,
    remote: plainfeed_git::Remote,
    now: OffsetDateTime,
) -> Result<bool, Error> {
    let Some(pending) = PendingPush::read_from(repository_root)? else {
        return Ok(false);
    };
    if pending.remote_url != remote.url() {
        write_conflict_report(
            repository_root,
            "pending state push belongs to a different remote URL",
            Vec::new(),
            Some(pending.previous_remote),
            None,
            now,
        )?;
        return Err(Error::ConflictActive {
            reason: "pending state push belongs to a different remote URL".to_owned(),
        });
    }
    let fetched =
        plainfeed_git::fetch(plainfeed_git::FetchRequest::main(repository_root, remote)).await?;
    if fetched.remote_tip == pending.previous_remote {
        plainfeed_git::delete_plainfeed_reference(repository_root, &pending.candidate_ref)?;
        PendingPush::clear(repository_root)?;
        return Ok(false);
    }
    if !plainfeed_git::is_ancestor(repository_root, &pending.pushed_commit, &fetched.remote_tip)?
        || fetched.state_tree.as_deref() != Some(pending.state_tree.as_str())
    {
        write_conflict_report(
            repository_root,
            "remote history cannot confirm the pending state push",
            Vec::new(),
            Some(pending.previous_remote),
            Some(fetched.remote_tip),
            now,
        )?;
        return Err(Error::ConflictActive {
            reason: "remote history cannot confirm the pending state push".to_owned(),
        });
    }

    let local_tip = plainfeed_git::reference_oid(repository_root, "refs/heads/main")?;
    if local_tip == pending.previous_remote {
        plainfeed_git::finalize_fast_forward_checkout(
            repository_root,
            "main",
            Some(&pending.previous_remote),
            &pending.pushed_commit,
        )?;
    } else if local_tip != pending.pushed_commit {
        write_conflict_report(
            repository_root,
            "local history cannot finalize the pending state push",
            Vec::new(),
            Some(local_tip),
            Some(fetched.remote_tip),
            now,
        )?;
        return Err(Error::ConflictActive {
            reason: "local history cannot finalize the pending state push".to_owned(),
        });
    }

    let mut state = SyncState::read_from(repository_root)?
        .unwrap_or_else(|| SyncState::new("origin", "refs/heads/main"));
    let pushed_commit = pending.pushed_commit.clone();
    let state_tree = pending.state_tree.clone();
    state.remote_url = Some(pending.remote_url.clone());
    finalize_publication_state(
        repository_root,
        &mut state,
        &pushed_commit,
        Some(state_tree.clone()),
        &pending.pushed_at,
    )?;
    DirtyJournal::new(repository_root).clear_snapshot(&pending.dirty_markers)?;
    plainfeed_git::delete_plainfeed_reference(repository_root, &pending.candidate_ref)?;
    PendingPush::clear(repository_root)?;
    if fetched.remote_tip != pushed_commit {
        activate_fetched_snapshot(ActivationRequest {
            repository_root: repository_root.to_owned(),
            branch: "main".to_owned(),
            expected_base: pushed_commit,
            remote_tip: fetched.remote_tip,
            trusted_state_tree: state_tree,
        })?;
    }
    Ok(true)
}

fn ensure_no_active_conflict(repository_root: &std::path::Path) -> Result<(), Error> {
    if let Some(report) = ConflictReport::read_from(repository_root)? {
        return Err(Error::ConflictActive {
            reason: report.reason,
        });
    }
    Ok(())
}

fn preflight_repository_contract(
    repository_root: &std::path::Path,
    now: OffsetDateTime,
) -> Result<(), Error> {
    if let Err(error) = plainfeed_git::validate_repository_contract(repository_root) {
        write_conflict_report(
            repository_root,
            error.to_string(),
            Vec::new(),
            None,
            None,
            now,
        )?;
        return Err(Error::Git(error));
    }
    Ok(())
}

fn trusted_state_tree(
    repository_root: &std::path::Path,
    local_tip: &str,
    recorded_tree: Option<&str>,
    now: OffsetDateTime,
) -> Result<String, Error> {
    let committed_tree = plainfeed_git::commit_root_entry_oid(repository_root, local_tip, "state")?;
    let Some(committed_tree) = committed_tree else {
        write_conflict_report(
            repository_root,
            "canonical local commit has no state tree",
            vec!["state".to_owned()],
            Some(local_tip.to_owned()),
            None,
            now,
        )?;
        return Err(Error::MissingStateTree);
    };
    if let Some(recorded_tree) = recorded_tree
        && recorded_tree != committed_tree
    {
        write_conflict_report(
            repository_root,
            "canonical local state tree differs from the recorded trusted tree",
            vec!["state".to_owned()],
            Some(local_tip.to_owned()),
            None,
            now,
        )?;
        return Err(Error::Audit(
            plainfeed_sync_core::AuditError::RemoteStateChanged {
                remote: Some(committed_tree),
                trusted: Some(recorded_tree.to_owned()),
            },
        ));
    }
    Ok(recorded_tree.unwrap_or(&committed_tree).to_owned())
}

fn preflight_remote_history(
    repository_root: &std::path::Path,
    local_tip: &str,
    remote_tip: &str,
    now: OffsetDateTime,
) -> Result<(), Error> {
    if plainfeed_git::is_ancestor(repository_root, local_tip, remote_tip)? {
        return Ok(());
    }
    let paths = plainfeed_git::changed_paths(repository_root, Some(local_tip), remote_tip)
        .unwrap_or_default();
    let error = plainfeed_git::Error::NonFastForward {
        base: local_tip.to_owned(),
        remote: remote_tip.to_owned(),
    };
    write_conflict_report(
        repository_root,
        error.to_string(),
        paths,
        Some(local_tip.to_owned()),
        Some(remote_tip.to_owned()),
        now,
    )?;
    Err(Error::Git(error))
}

fn preflight_local_ownership(
    repository_root: &std::path::Path,
    local_tip: &str,
    now: OffsetDateTime,
) -> Result<(), Error> {
    let paths =
        plainfeed_git::worktree_changed_paths(repository_root, local_tip, &["content", "config"])?;
    if paths.is_empty() {
        return Ok(());
    }
    write_conflict_report(
        repository_root,
        "local content or configuration differs from the canonical Git commit",
        paths.clone(),
        Some(local_tip.to_owned()),
        None,
        now,
    )?;
    Err(Error::LocalOwnership { paths })
}

fn write_conflict_report(
    repository_root: &std::path::Path,
    reason: impl Into<String>,
    paths: Vec<String>,
    local_base: Option<String>,
    remote_tip: Option<String>,
    now: OffsetDateTime,
) -> Result<(), plainfeed_sync_core::Error> {
    let detected_at = now
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    let mut report = ConflictReport::new(reason, detected_at);
    report.paths = paths;
    report.local_base = local_base;
    report.remote_tip = remote_tip;
    report.write_to(repository_root)
}

fn finalize_publication_state(
    repository_root: &std::path::Path,
    state: &mut SyncState,
    remote_tip: &str,
    state_tree: Option<String>,
    timestamp: &str,
) -> Result<(), plainfeed_sync_core::Error> {
    state.last_remote_oid = Some(remote_tip.to_owned());
    state.last_state_tree_oid = state_tree;
    state.last_pull_at = Some(timestamp.to_owned());
    state.last_push_at = Some(timestamp.to_owned());
    state.last_error = None;
    state.write_to(repository_root)
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
    )?;
    let Some(remote_state_tree) = remote_state_tree else {
        write_conflict_report(
            &request.repository_root,
            "remote commit has no state tree",
            changed_paths.clone(),
            Some(request.expected_base.clone()),
            Some(request.remote_tip.clone()),
            OffsetDateTime::now_utc(),
        )?;
        return Err(Error::MissingStateTree);
    };
    if let Err(error) = audit_remote_changes(
        changed_paths.iter().map(String::as_str),
        Some(&remote_state_tree),
        Some(&request.trusted_state_tree),
    ) {
        write_conflict_report(
            &request.repository_root,
            error.to_string(),
            changed_paths.clone(),
            Some(request.expected_base.clone()),
            Some(request.remote_tip.clone()),
            OffsetDateTime::now_utc(),
        )?;
        return Err(Error::Audit(error));
    }

    let staging = request
        .repository_root
        .join(".plainfeed/staging")
        .join(&request.remote_tip);
    plainfeed_git::export_remote_snapshot(&request.repository_root, &request.remote_tip, &staging)?;
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    let previous_sync_state = SyncState::read_from(&request.repository_root)?;
    let mut next_sync_state = previous_sync_state
        .clone()
        .unwrap_or_else(|| SyncState::new("origin", format!("refs/heads/{}", request.branch)));
    next_sync_state.last_remote_oid = Some(request.remote_tip.clone());
    next_sync_state.last_state_tree_oid = Some(remote_state_tree);
    next_sync_state.last_pull_at = Some(timestamp.clone());
    next_sync_state.last_error = None;
    let result = activate_staged_snapshot_with_finalize(
        &request.repository_root,
        &staging,
        |snapshot| Store::open(snapshot).validate(),
        || {
            next_sync_state
                .write_to(&request.repository_root)
                .map_err(|error| error.to_string())?;
            if let Err(error) = plainfeed_git::finalize_fast_forward_checkout(
                &request.repository_root,
                &request.branch,
                Some(&request.expected_base),
                &request.remote_tip,
            ) {
                restore_sync_state(&request.repository_root, previous_sync_state.as_ref())
                    .map_err(|restore| {
                        format!("{error}; also failed to restore sync state: {restore}")
                    })?;
                return Err(error.to_string());
            }
            Ok(())
        },
    );
    if let Err(error) = result {
        let mut report = ConflictReport::new(error.to_string(), timestamp);
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

fn restore_sync_state(
    repository_root: &std::path::Path,
    previous: Option<&SyncState>,
) -> Result<(), plainfeed_sync_core::Error> {
    match previous {
        Some(previous) => previous.write_to(repository_root),
        None => {
            let path = repository_root.join(".plainfeed/sync.toml");
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(plainfeed_sync_core::Error::Io { path, source }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    use super::{
        ActivationRequest, SyncCommand, SyncState, activate_fetched_snapshot,
        ensure_no_active_conflict, preflight_local_ownership, preflight_remote_history,
        preflight_repository_contract, pull_is_due, recover_local_transition,
        state_publication_is_due,
    };

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
    fn pull_schedule_distinguishes_tick_force_and_status() {
        let now = time::OffsetDateTime::parse(
            "2026-07-17T01:10:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(pull_is_due(
            SyncCommand::Force,
            Some("2026-07-17T01:09:59Z"),
            now
        ));
        assert!(!pull_is_due(SyncCommand::Status, None, now));
        assert!(pull_is_due(SyncCommand::Tick, None, now));
        assert!(!pull_is_due(
            SyncCommand::Tick,
            Some("2026-07-17T01:05:01Z"),
            now
        ));
        assert!(pull_is_due(
            SyncCommand::Tick,
            Some("2026-07-17T01:05:00Z"),
            now
        ));
    }

    #[test]
    fn state_publication_batches_idle_markers_but_caps_continuous_mutation() {
        let now = time::OffsetDateTime::from_unix_timestamp(600).unwrap();
        let marker = |seconds: u128, suffix: &str| {
            format!(
                "{:032x}-0000000000000000-{suffix}.toml",
                seconds * 1_000_000_000
            )
        };
        assert!(!state_publication_is_due(SyncCommand::Tick, &[], now));
        assert!(state_publication_is_due(
            SyncCommand::Force,
            &[marker(599, "new")],
            now
        ));
        assert!(!state_publication_is_due(
            SyncCommand::Tick,
            &[marker(571, "active")],
            now
        ));
        assert!(state_publication_is_due(
            SyncCommand::Tick,
            &[marker(570, "idle")],
            now
        ));
        assert!(state_publication_is_due(
            SyncCommand::Tick,
            &[marker(300, "old"), marker(599, "new")],
            now
        ));
    }

    #[test]
    fn an_active_conflict_requires_explicit_acknowledgement() {
        let temporary = tempfile::tempdir().unwrap();
        let report = plainfeed_sync_core::ConflictReport::new(
            "remote state changed",
            "2026-07-17T05:00:00Z",
        );
        report.write_to(temporary.path()).unwrap();

        assert!(ensure_no_active_conflict(temporary.path()).is_err());
        plainfeed_sync_core::ConflictReport::clear(temporary.path()).unwrap();
        ensure_no_active_conflict(temporary.path()).unwrap();
    }

    #[test]
    fn an_invalid_local_repository_shape_is_reported() {
        let temporary = tempfile::tempdir().unwrap();
        gix::init(temporary.path()).unwrap();

        assert!(
            preflight_repository_contract(
                temporary.path(),
                time::OffsetDateTime::from_unix_timestamp(1_784_160_000).unwrap(),
            )
            .is_err()
        );
        let report = plainfeed_sync_core::ConflictReport::read_from(temporary.path())
            .unwrap()
            .unwrap();
        assert!(report.reason.contains("refs/heads/main"));
    }

    #[test]
    fn divergent_history_pauses_before_activation() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("data")).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let tree = repository.empty_tree().id;
        let base = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/base",
                "base",
                tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        let local = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/main",
                "local",
                tree,
                [base.detach()],
            )
            .unwrap();
        let remote = repository
            .commit_as(
                identity,
                identity,
                "refs/remotes/origin/main",
                "remote",
                tree,
                [base.detach()],
            )
            .unwrap();

        assert!(
            preflight_remote_history(
                repository.path(),
                &local.to_string(),
                &remote.to_string(),
                time::OffsetDateTime::from_unix_timestamp(1_784_160_000).unwrap(),
            )
            .is_err()
        );
        let report = plainfeed_sync_core::ConflictReport::read_from(repository.path())
            .unwrap()
            .unwrap();
        assert_eq!(report.local_base, Some(local.to_string()));
        assert_eq!(report.remote_tip, Some(remote.to_string()));
    }

    #[test]
    fn local_content_changes_pause_sync_and_write_a_conflict_report() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("data");
        let repository = gix::init(&root).unwrap();
        let content = repository.write_blob(b"committed\n").unwrap();
        let config = repository.write_blob(CHANNELS.as_bytes()).unwrap();
        let mut editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        editor
            .upsert("content/item.md", EntryKind::Blob, content)
            .unwrap();
        editor
            .upsert("config/channels.toml", EntryKind::Blob, config)
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
                "refs/heads/main",
                "fixture",
                tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        fs::create_dir_all(root.join("content")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("content/item.md"), b"manual local edit\n").unwrap();
        fs::write(root.join("config/channels.toml"), CHANNELS).unwrap();

        assert!(
            preflight_local_ownership(
                &root,
                &commit.to_string(),
                time::OffsetDateTime::from_unix_timestamp(1_784_160_000).unwrap(),
            )
            .is_err()
        );
        let report = plainfeed_sync_core::ConflictReport::read_from(&root)
            .unwrap()
            .unwrap();
        assert_eq!(report.paths, ["content/item.md"]);
        assert_eq!(report.local_base, Some(commit.to_string()));
    }

    #[test]
    fn explicit_local_recovery_rolls_back_an_interrupted_activation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("data");
        let repository = gix::init(&root).unwrap();
        let content = repository.write_blob(OLD_ENTRY.as_bytes()).unwrap();
        let config = repository.write_blob(CHANNELS.as_bytes()).unwrap();
        let mut editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        editor
            .upsert("content/old-entry.md", EntryKind::Blob, content)
            .unwrap();
        editor
            .upsert("config/channels.toml", EntryKind::Blob, config)
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
                "refs/heads/main",
                "fixture",
                tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        fs::create_dir_all(root.join("content")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("content/old-entry.md"), OLD_ENTRY).unwrap();
        fs::write(root.join("config/channels.toml"), CHANNELS).unwrap();
        let backup = root.join(".plainfeed/backup/activation-test");
        fs::create_dir_all(&backup).unwrap();
        fs::create_dir_all(root.join(".plainfeed/update.lock")).unwrap();
        fs::rename(root.join("content"), backup.join("content")).unwrap();
        fs::rename(root.join("config"), backup.join("config")).unwrap();
        fs::create_dir_all(root.join("content")).unwrap();
        fs::write(root.join("content/partial.md"), "partial activation").unwrap();

        assert!(recover_local_transition(&root).unwrap());
        assert_eq!(
            fs::read_to_string(root.join("content/old-entry.md")).unwrap(),
            OLD_ENTRY
        );
        assert_eq!(
            fs::read_to_string(root.join("config/channels.toml")).unwrap(),
            CHANNELS
        );
        assert!(!root.join(".plainfeed/update.lock").exists());
        assert!(!root.join(".plainfeed/backup").exists());
        assert_eq!(
            SyncState::read_from(&root)
                .unwrap()
                .unwrap()
                .last_remote_oid,
            Some(commit.to_string())
        );
    }

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
            trusted_state_tree: trusted_state_tree.clone(),
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
        let sync_state = plainfeed_sync_core::SyncState::read_from(&root)
            .unwrap()
            .unwrap();
        assert_eq!(
            sync_state.last_remote_oid.as_deref(),
            Some(new_commit.to_string().as_str())
        );
        assert_eq!(
            sync_state.last_state_tree_oid.as_deref(),
            Some(trusted_state_tree.as_str())
        );
        assert!(sync_state.last_pull_at.is_some());
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

    #[test]
    fn remote_state_change_is_reported_without_activation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("data");
        let repository = gix::init(&root).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let content = repository.write_blob(OLD_ENTRY.as_bytes()).unwrap();
        let old_state = repository.write_blob(STATE.as_bytes()).unwrap();
        let mut old_editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        old_editor
            .upsert("content/old-entry.md", EntryKind::Blob, content)
            .unwrap();
        old_editor
            .upsert("state/entries/old-entry.toml", EntryKind::Blob, old_state)
            .unwrap();
        let old_tree = old_editor.write().unwrap();
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
        let changed_state = repository
            .write_blob(
                STATE
                    .replace("favorite = true", "favorite = false")
                    .as_bytes(),
            )
            .unwrap();
        let mut remote_editor = repository.edit_tree(old_tree).unwrap();
        remote_editor
            .upsert(
                "state/entries/old-entry.toml",
                EntryKind::Blob,
                changed_state,
            )
            .unwrap();
        let remote_tree = remote_editor.write().unwrap();
        let remote_commit = repository
            .commit_as(
                identity,
                identity,
                "refs/remotes/origin/main",
                "remote state edit",
                remote_tree,
                [old_commit.detach()],
            )
            .unwrap();
        let trusted_state_tree =
            plainfeed_git::commit_root_entry_oid(&root, &old_commit.to_string(), "state")
                .unwrap()
                .unwrap();

        let error = activate_fetched_snapshot(ActivationRequest {
            repository_root: root.clone(),
            branch: "main".to_owned(),
            expected_base: old_commit.to_string(),
            remote_tip: remote_commit.to_string(),
            trusted_state_tree,
        })
        .unwrap_err();

        assert!(matches!(error, super::Error::Audit(_)));
        let report = plainfeed_sync_core::ConflictReport::read_from(&root)
            .unwrap()
            .unwrap();
        assert_eq!(report.paths, ["state/entries/old-entry.toml"]);
        assert_eq!(report.remote_tip, Some(remote_commit.to_string()));
    }
}
