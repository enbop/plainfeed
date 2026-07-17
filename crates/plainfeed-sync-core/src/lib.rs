//! Provider-independent synchronization policy for Plainfeed data repositories.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SYNC_FORMAT: &str = "plainfeed.sync/v1";
pub const CONFLICT_FORMAT: &str = "plainfeed.conflict/v1";

static MARKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathOwner {
    Producer,
    RepositoryOwner,
    Plainfeed,
    LocalOnly,
    Unknown,
}

pub fn path_owner(path: &Path) -> PathOwner {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return PathOwner::Unknown;
    };
    let remainder: Vec<_> = components.collect();
    if remainder
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return PathOwner::Unknown;
    }

    match first.to_str() {
        Some("content") => PathOwner::Producer,
        Some("config") => PathOwner::RepositoryOwner,
        Some(".gitignore") if remainder.is_empty() => PathOwner::RepositoryOwner,
        Some("state") => PathOwner::Plainfeed,
        Some(".plainfeed") => PathOwner::LocalOnly,
        _ => PathOwner::Unknown,
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AuditError {
    #[error("remote state tree {remote:?} does not match trusted tree {trusted:?}")]
    RemoteStateChanged {
        remote: Option<String>,
        trusted: Option<String>,
    },
    #[error("remote path is outside the repository contract: {path}")]
    ForbiddenPath { path: String },
    #[error("Plainfeed may not publish non-state path: {path}")]
    NotPlainfeedOwned { path: String },
}

pub fn audit_remote_changes<I, S>(
    paths: I,
    remote_state_tree: Option<&str>,
    trusted_state_tree: Option<&str>,
) -> Result<(), AuditError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if remote_state_tree != trusted_state_tree || trusted_state_tree.is_none() {
        return Err(AuditError::RemoteStateChanged {
            remote: remote_state_tree.map(ToOwned::to_owned),
            trusted: trusted_state_tree.map(ToOwned::to_owned),
        });
    }

    for path in paths {
        let path = path.as_ref();
        match path_owner(Path::new(path)) {
            PathOwner::Producer | PathOwner::RepositoryOwner | PathOwner::Plainfeed => {}
            PathOwner::LocalOnly | PathOwner::Unknown => {
                return Err(AuditError::ForbiddenPath {
                    path: path.to_owned(),
                });
            }
        }
    }
    Ok(())
}

pub fn audit_plainfeed_changes<I, S>(paths: I) -> Result<(), AuditError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for path in paths {
        let path = path.as_ref();
        if path_owner(Path::new(path)) != PathOwner::Plainfeed {
            return Err(AuditError::NotPlainfeedOwned {
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error("cannot serialize synchronization metadata: {0}")]
    Toml(#[from] toml::ser::Error),
}

#[derive(Clone, Debug)]
pub struct DirtyJournal {
    directory: PathBuf,
}

impl DirtyJournal {
    pub fn new(repository_root: impl AsRef<Path>) -> Self {
        Self {
            directory: repository_root.as_ref().join(".plainfeed/dirty"),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn mark(&self, entry_id: &str) -> Result<String, Error> {
        fs::create_dir_all(&self.directory).map_err(|source| Error::Io {
            path: self.directory.clone(),
            source,
        })?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Clock)?
            .as_nanos();
        let safe_id: String = entry_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect();

        loop {
            let sequence = MARKER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!("{timestamp:032x}-{sequence:016x}-{safe_id}.toml");
            let path = self.directory.join(&name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let marker = DirtyMarker { entry_id };
                    file.write_all(toml::to_string(&marker)?.as_bytes())
                        .map_err(|source| Error::Io {
                            path: path.clone(),
                            source,
                        })?;
                    file.sync_all().map_err(|source| Error::Io {
                        path: path.clone(),
                        source,
                    })?;
                    return Ok(name);
                }
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(Error::Io { path, source }),
            }
        }
    }

    pub fn snapshot(&self) -> Result<Vec<String>, Error> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::Io {
                    path: self.directory.clone(),
                    source,
                });
            }
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io {
                path: self.directory.clone(),
                source,
            })?;
            if entry
                .file_type()
                .map_err(|source| Error::Io {
                    path: entry.path(),
                    source,
                })?
                .is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_owned());
            }
        }
        names.sort_unstable();
        Ok(names)
    }

    pub fn clear_snapshot(&self, names: &[String]) -> Result<(), Error> {
        for name in names {
            if Path::new(name).components().count() != 1 {
                continue;
            }
            let path = self.directory.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path, source }),
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct DirtyMarker<'a> {
    entry_id: &'a str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncState {
    pub format: String,
    pub remote: String,
    pub branch: String,
    pub last_remote_oid: Option<String>,
    pub last_state_tree_oid: Option<String>,
    pub last_pull_at: Option<String>,
    pub last_push_at: Option<String>,
    pub last_error: Option<String>,
}

impl SyncState {
    pub fn new(remote: impl Into<String>, branch: impl Into<String>) -> Self {
        Self {
            format: SYNC_FORMAT.to_owned(),
            remote: remote.into(),
            branch: branch.into(),
            last_remote_oid: None,
            last_state_tree_oid: None,
            last_pull_at: None,
            last_push_at: None,
            last_error: None,
        }
    }

    pub fn write_to(&self, repository_root: impl AsRef<Path>) -> Result<(), Error> {
        write_metadata(repository_root.as_ref(), "sync.toml", self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConflictReport {
    pub format: String,
    pub reason: String,
    pub paths: Vec<String>,
    pub local_base: Option<String>,
    pub remote_tip: Option<String>,
    pub detected_at: String,
}

impl ConflictReport {
    pub fn new(reason: impl Into<String>, detected_at: impl Into<String>) -> Self {
        Self {
            format: CONFLICT_FORMAT.to_owned(),
            reason: reason.into(),
            paths: Vec::new(),
            local_base: None,
            remote_tip: None,
            detected_at: detected_at.into(),
        }
    }

    pub fn to_toml(&self) -> Result<String, Error> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn write_to(&self, repository_root: impl AsRef<Path>) -> Result<(), Error> {
        write_metadata(repository_root.as_ref(), "conflict.toml", self)
    }
}

fn write_metadata(
    repository_root: &Path,
    file_name: &str,
    value: &impl Serialize,
) -> Result<(), Error> {
    let directory = repository_root.join(".plainfeed");
    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        path: directory.clone(),
        source,
    })?;
    let destination = directory.join(file_name);
    let sequence = MARKER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{file_name}.{sequence:016x}.tmp"));
    let text = toml::to_string_pretty(value)?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, &destination).map_err(|source| Error::Io {
            path: destination.clone(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{
        AuditError, CONFLICT_FORMAT, ConflictReport, DirtyJournal, PathOwner, SyncState,
        audit_plainfeed_changes, audit_remote_changes, path_owner,
    };

    #[test]
    fn classifies_repository_paths_by_owner() {
        assert_eq!(
            path_owner(Path::new("content/2026/item.md")),
            PathOwner::Producer
        );
        assert_eq!(
            path_owner(Path::new("config/channels.toml")),
            PathOwner::RepositoryOwner
        );
        assert_eq!(
            path_owner(Path::new("state/entries/item.toml")),
            PathOwner::Plainfeed
        );
        assert_eq!(
            path_owner(Path::new(".plainfeed/sync.toml")),
            PathOwner::LocalOnly
        );
        assert_eq!(path_owner(Path::new("README.md")), PathOwner::Unknown);
        assert_eq!(
            path_owner(Path::new("content/../state/item.toml")),
            PathOwner::Unknown
        );
    }

    #[test]
    fn accepts_remote_content_and_config_with_the_trusted_state_tree() {
        audit_remote_changes(
            ["content/new.md", "config/channels.toml"],
            Some("state-tree-a"),
            Some("state-tree-a"),
        )
        .unwrap();
    }

    #[test]
    fn rejects_remote_state_or_local_metadata_changes() {
        assert!(matches!(
            audit_remote_changes(["state/entries/item.toml"], Some("new"), Some("old")),
            Err(AuditError::RemoteStateChanged { .. })
        ));
        assert!(matches!(
            audit_remote_changes([".plainfeed/sync.toml"], Some("same"), Some("same")),
            Err(AuditError::ForbiddenPath { .. })
        ));
    }

    #[test]
    fn plainfeed_may_publish_only_state_paths() {
        audit_plainfeed_changes(["state/entries/item.toml"]).unwrap();
        assert!(matches!(
            audit_plainfeed_changes(["content/item.md"]),
            Err(AuditError::NotPlainfeedOwned { .. })
        ));
    }

    #[test]
    fn clearing_a_snapshot_preserves_markers_created_during_sync() {
        let temporary = tempfile::tempdir().unwrap();
        let journal = DirtyJournal::new(temporary.path());
        let first = journal.mark("item-a").unwrap();
        let snapshot = journal.snapshot().unwrap();
        assert_eq!(snapshot, vec![first]);

        let second = journal.mark("item-b").unwrap();
        journal.clear_snapshot(&snapshot).unwrap();

        assert_eq!(journal.snapshot().unwrap(), vec![second]);
    }

    #[test]
    fn marker_creation_does_not_overwrite_an_existing_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let journal = DirtyJournal::new(temporary.path());
        let first = journal.mark("same-item").unwrap();
        let second = journal.mark("same-item").unwrap();

        assert_ne!(first, second);
        assert_eq!(fs::read_dir(journal.directory()).unwrap().count(), 2);
    }

    #[test]
    fn conflict_report_is_versioned_human_readable_toml() {
        let mut report = ConflictReport::new("remote state changed", "2026-07-17T00:00:00Z");
        report.paths.push("state/entries/item.toml".to_owned());
        report.local_base = Some("aaa".to_owned());
        report.remote_tip = Some("bbb".to_owned());

        let encoded = report.to_toml().unwrap();
        let decoded: ConflictReport = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, report);
        assert_eq!(decoded.format, CONFLICT_FORMAT);
        assert!(encoded.contains("reason = \"remote state changed\""));
    }

    #[test]
    fn writes_local_status_and_conflict_files_atomically() {
        let temporary = tempfile::tempdir().unwrap();
        let mut state = SyncState::new("origin", "refs/heads/main");
        state.last_remote_oid = Some("aaa".to_owned());
        state.write_to(temporary.path()).unwrap();

        let report = ConflictReport::new("diverged history", "2026-07-17T00:00:00Z");
        report.write_to(temporary.path()).unwrap();

        let sync_text = fs::read_to_string(temporary.path().join(".plainfeed/sync.toml")).unwrap();
        let conflict_text =
            fs::read_to_string(temporary.path().join(".plainfeed/conflict.toml")).unwrap();
        assert!(sync_text.contains("format = \"plainfeed.sync/v1\""));
        assert!(sync_text.contains("last_remote_oid = \"aaa\""));
        assert!(conflict_text.contains("format = \"plainfeed.conflict/v1\""));
        assert!(
            fs::read_dir(temporary.path().join(".plainfeed"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
        );
    }
}
