use std::{env, error::Error, path::PathBuf};

use plainfeed_git::{Credentials, Remote};
use plainfeed_sync::{
    PublishOutcome, SyncCommand, publish_state, run_pull_cycle, state_publication_is_due,
};
use plainfeed_sync_core::{ConflictReport, DirtyJournal, SyncState};
use time::OffsetDateTime;

fn main() -> Result<(), Box<dyn Error>> {
    rustls_rustcrypto::provider()
        .install_default()
        .map_err(|_| "failed to install the RustCrypto TLS provider")?;
    let mut arguments = env::args().skip(1);
    let action = arguments.next();
    let command = match action.as_deref() {
        Some("tick") => SyncCommand::Tick,
        Some("force") => SyncCommand::Force,
        Some("status") => SyncCommand::Status,
        Some("acknowledge-conflict") => {
            let repository_root =
                PathBuf::from(arguments.next().unwrap_or_else(|| "/data".to_owned()));
            if arguments.next().is_some() {
                return Err(usage().into());
            }
            ConflictReport::clear(&repository_root)?;
            println!("conflict=acknowledged");
            return Ok(());
        }
        _ => return Err(usage().into()),
    };
    let repository_root = PathBuf::from(arguments.next().unwrap_or_else(|| "/data".to_owned()));
    if arguments.next().is_some() {
        return Err(usage().into());
    }
    if command == SyncCommand::Status {
        return print_status(&repository_root);
    }

    let state = SyncState::read_from(&repository_root)?;
    let remote_url = env::var("PLAINFEED_REMOTE_URL")
        .ok()
        .or_else(|| state.as_ref().and_then(|state| state.remote_url.clone()))
        .ok_or("PLAINFEED_REMOTE_URL is required until sync.toml records a remote_url")?;
    let credentials = credentials_from_environment();
    let remote = Remote::new(remote_url, credentials)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let now = OffsetDateTime::now_utc();
    let markers = DirtyJournal::new(&repository_root).snapshot()?;
    let result = if state_publication_is_due(command, &markers, now) {
        runtime
            .block_on(publish_state(&repository_root, remote, now))
            .map(|outcome| {
                let label = match outcome {
                    PublishOutcome::NoDirtyState => "no-dirty-state",
                    PublishOutcome::AlreadyPublished => "already-published",
                    PublishOutcome::Pushed(_) => "completed",
                };
                ("completed", label)
            })
    } else {
        runtime
            .block_on(run_pull_cycle(command, &repository_root, remote, now))
            .map(|ran| (if ran { "completed" } else { "not-due" }, "not-due"))
    };
    let (pull, push) = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            persist_error(&repository_root, &error.to_string());
            return Err(error.into());
        }
    };
    println!("pull={pull}");
    println!("push={push}");
    Ok(())
}

fn usage() -> &'static str {
    "usage: plainfeed-sync <tick|force|status|acknowledge-conflict> [DATA_ROOT]"
}

fn persist_error(repository_root: &PathBuf, error: &str) {
    let mut state = match SyncState::read_from(repository_root) {
        Ok(Some(state)) => state,
        Ok(None) => SyncState::new("origin", "refs/heads/main"),
        Err(_) => return,
    };
    state.last_error = Some(error.to_owned());
    let _ = state.write_to(repository_root);
}

fn status_value(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\0' => ' ',
            _ => character,
        })
        .collect()
}

fn print_conflict_status(repository_root: &PathBuf) -> Result<(), Box<dyn Error>> {
    match ConflictReport::read_from(repository_root)? {
        Some(report) => {
            println!("conflict_active=true");
            println!("conflict_reason={}", status_value(&report.reason));
            println!("conflict_paths={}", report.paths.join(","));
            println!(
                "conflict_local_base={}",
                report.local_base.as_deref().unwrap_or("")
            );
            println!(
                "conflict_remote_tip={}",
                report.remote_tip.as_deref().unwrap_or("")
            );
            println!("conflict_detected_at={}", report.detected_at);
        }
        None => println!("conflict_active=false"),
    }
    Ok(())
}

fn credentials_from_environment() -> Option<Credentials> {
    if let Ok(password) = env::var("PLAINFEED_GIT_PASSWORD") {
        let username = env::var("PLAINFEED_GIT_USERNAME").unwrap_or_else(|_| "git".to_owned());
        return Some(Credentials::basic(username, password));
    }
    env::var("PLAINFEED_GITHUB_TOKEN")
        .ok()
        .map(|token| Credentials::basic("x-access-token", token))
}

fn print_status(repository_root: &PathBuf) -> Result<(), Box<dyn Error>> {
    let state = SyncState::read_from(repository_root)?;
    println!("format=plainfeed.sync-status/v1");
    match state {
        Some(state) => {
            println!("remote={}", state.remote);
            println!("remote_url={}", state.remote_url.as_deref().unwrap_or(""));
            println!("branch={}", state.branch);
            println!(
                "last_remote_oid={}",
                state.last_remote_oid.as_deref().unwrap_or("")
            );
            println!(
                "last_pull_at={}",
                state.last_pull_at.as_deref().unwrap_or("")
            );
            println!(
                "last_push_at={}",
                state.last_push_at.as_deref().unwrap_or("")
            );
            println!("last_error={}", state.last_error.as_deref().unwrap_or(""));
        }
        None => println!("state=uninitialized"),
    }
    println!(
        "dirty_markers={}",
        DirtyJournal::new(repository_root).snapshot()?.len()
    );
    print_conflict_status(repository_root)?;
    Ok(())
}
