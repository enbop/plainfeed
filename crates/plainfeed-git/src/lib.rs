//! A narrow, provider-independent Git synchronization adapter for WASIp2.

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use thiserror::Error;

mod fetch;
mod http;
mod push;
mod state_commit;

pub use fetch::{
    FetchOutcome, FetchRequest, changed_paths, commit_root_entry_oid, delete_plainfeed_reference,
    export_initial_snapshot, export_remote_snapshot, fetch, finalize_fast_forward_checkout,
    is_ancestor, reference_exists, reference_oid, set_head_branch, validate_repository_contract,
    worktree_changed_paths,
};
pub use push::{PushOutcome, push_one_commit};
pub use state_commit::{StateCommit, create_state_commit};

#[derive(Clone)]
pub struct Credentials {
    username: String,
    password: String,
}

impl Credentials {
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }

    fn parts(&self) -> (&str, &str) {
        (&self.username, &self.password)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct Remote {
    url: String,
    credentials: Option<Credentials>,
}

impl Remote {
    pub fn new(url: impl Into<String>, credentials: Option<Credentials>) -> Result<Self, Error> {
        let url = url.into();
        let parsed = reqwest::Url::parse(&url).map_err(|source| Error::InvalidUrl {
            url: url.clone(),
            source,
        })?;
        if credentials.is_some() && parsed.scheme() != "https" {
            return Err(Error::InsecureAuthenticatedUrl);
        }
        Ok(Self { url, credentials })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchLimits {
    pub max_response_bytes: usize,
    pub max_repository_bytes: u64,
    pub max_object_count: usize,
    pub max_object_bytes: usize,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: 64 * 1024 * 1024,
            max_repository_bytes: 256 * 1024 * 1024,
            max_object_count: 100_000,
            max_object_bytes: 16 * 1024 * 1024,
        }
    }
}

impl FetchLimits {
    pub fn check_repository(&self, path: impl AsRef<Path>) -> Result<u64, Error> {
        let path = path.as_ref();
        let bytes = directory_bytes(path)?;
        if bytes > self.max_repository_bytes {
            return Err(Error::RepositoryTooLarge {
                bytes,
                limit: self.max_repository_bytes,
            });
        }
        Ok(bytes)
    }

    pub fn check_object_count(&self, count: usize) -> Result<(), Error> {
        if count > self.max_object_count {
            return Err(Error::TooManyObjects {
                count,
                limit: self.max_object_count,
            });
        }
        Ok(())
    }

    pub fn check_object_size(&self, bytes: usize) -> Result<(), Error> {
        if bytes > self.max_object_bytes {
            return Err(Error::ObjectTooLarge {
                bytes,
                limit: self.max_object_bytes,
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct BufferLimit {
    bytes: Vec<u8>,
    limit: usize,
}

impl BufferLimit {
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    pub fn extend(&mut self, chunk: &[u8]) -> Result<(), Error> {
        let attempted = self.bytes.len().saturating_add(chunk.len());
        if attempted > self.limit {
            return Err(Error::ResponseTooLarge {
                bytes: attempted,
                limit: self.limit,
            });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("authenticated Git remotes must use HTTPS")]
    InsecureAuthenticatedUrl,
    #[error("invalid Git remote URL {url:?}: {source}")]
    InvalidUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },
    #[error("Git HTTP response would use {bytes} bytes, over the {limit}-byte limit")]
    ResponseTooLarge { bytes: usize, limit: usize },
    #[error("Git repository uses {bytes} bytes, over the {limit}-byte limit")]
    RepositoryTooLarge { bytes: u64, limit: u64 },
    #[error("cannot inspect repository storage at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Git operation failed: {0}")]
    Git(String),
    #[error("remote tip {remote} is not a descendant of local base {base}")]
    NonFastForward { base: String, remote: String },
    #[error("push candidate parent {parent} does not match advertised remote tip {remote}")]
    StaleRemote { parent: String, remote: String },
    #[error("generated push pack uses {bytes} bytes, over the {limit}-byte limit")]
    PushTooLarge { bytes: usize, limit: usize },
    #[error("snapshot contains {count} objects, over the {limit}-object limit")]
    TooManyObjects { count: usize, limit: usize },
    #[error("Git object uses {bytes} bytes, over the {limit}-byte object limit")]
    ObjectTooLarge { bytes: usize, limit: usize },
    #[error("remote does not advertise required ref {name}")]
    MissingRemoteRef { name: String },
    #[error("local repository does not contain required ref {name}")]
    MissingLocalRef { name: String },
    #[error("unsupported repository object format {actual}; Plainfeed v1 requires sha1")]
    UnsupportedObjectFormat { actual: String },
    #[error("remote object format {remote} is incompatible with local format {local}")]
    IncompatibleObjectFormat { local: String, remote: String },
}

impl Error {
    pub fn is_repository_contract_violation(&self) -> bool {
        matches!(
            self,
            Self::MissingRemoteRef { .. }
                | Self::MissingLocalRef { .. }
                | Self::UnsupportedObjectFormat { .. }
                | Self::IncompatibleObjectFormat { .. }
        )
    }
}

fn directory_bytes(path: &Path) -> Result<u64, Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let entries = fs::read_dir(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })?;
    let mut bytes = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        bytes = bytes.saturating_add(directory_bytes(&entry.path())?);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{fmt::Write as _, fs};

    use super::{BufferLimit, Credentials, FetchLimits, Remote};

    #[test]
    fn credentials_are_redacted_and_require_https() {
        let credentials = Credentials::basic("plainfeed", "secret-token");
        let mut debug = String::new();
        write!(&mut debug, "{credentials:?}").unwrap();
        assert!(!debug.contains("secret-token"));

        assert!(Remote::new("https://github.com/example/private.git", Some(credentials)).is_ok());
        assert!(
            Remote::new(
                "http://github.com/example/private.git",
                Some(Credentials::basic("plainfeed", "secret-token")),
            )
            .is_err()
        );
    }

    #[test]
    fn response_buffer_stops_before_exceeding_the_limit() {
        let mut buffer = BufferLimit::new(5);
        buffer.extend(b"abc").unwrap();
        assert!(buffer.extend(b"def").is_err());
        assert_eq!(buffer.as_slice(), b"abc");
    }

    #[test]
    fn repository_usage_limit_counts_files_without_following_symlinks() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join("objects/pack")).unwrap();
        fs::write(temporary.path().join("objects/one"), b"1234").unwrap();
        fs::write(temporary.path().join("objects/pack/two"), b"56789").unwrap();

        let limits = FetchLimits {
            max_response_bytes: 32,
            max_repository_bytes: 9,
            max_object_count: 2,
            max_object_bytes: 5,
        };
        assert_eq!(limits.check_repository(temporary.path()).unwrap(), 9);
        fs::write(temporary.path().join("objects/three"), b"x").unwrap();
        assert!(limits.check_repository(temporary.path()).is_err());
        limits.check_object_count(2).unwrap();
        assert!(limits.check_object_count(3).is_err());
        limits.check_object_size(5).unwrap();
        assert!(limits.check_object_size(6).is_err());
    }
}
