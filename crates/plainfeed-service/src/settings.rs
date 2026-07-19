use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FORMAT: &str = "plainfeed.service-settings/v1";
const FILE_NAME: &str = "service-settings.toml";

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceSettings {
    pub format: String,
    pub remote_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_token: Option<String>,
}

impl ServiceSettings {
    pub fn new(remote_url: String, github_token: Option<String>) -> Self {
        Self {
            format: FORMAT.to_owned(),
            remote_url,
            github_token,
        }
    }

    pub fn read_from(data_root: &Path) -> Result<Option<Self>, Error> {
        let path = settings_path(data_root);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::Io { path, source }),
        };
        let settings: Self = toml::from_str(&source).map_err(|source| Error::Toml {
            path: path.clone(),
            source,
        })?;
        if settings.format != FORMAT {
            return Err(Error::UnsupportedFormat {
                path,
                format: settings.format,
            });
        }
        Ok(Some(settings))
    }

    pub fn write_to(&self, data_root: &Path) -> Result<(), Error> {
        let metadata = data_root.join(".plainfeed");
        fs::create_dir_all(&metadata).map_err(|source| Error::Io {
            path: metadata.clone(),
            source,
        })?;
        let path = settings_path(data_root);
        let temporary = metadata.join(format!(".{FILE_NAME}.tmp"));
        let encoded = toml::to_string_pretty(self)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(encoded.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, &path).map_err(|source| Error::Io { path, source })
    }

    pub fn has_token(&self) -> bool {
        self.github_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
    }
}

pub fn settings_path(data_root: &Path) -> PathBuf {
    data_root.join(".plainfeed").join(FILE_NAME)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported service settings format {format} in {path}")]
    UnsupportedFormat { path: PathBuf, format: String },
    #[error("failed to encode service settings: {0}")]
    Encode(#[from] toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_without_writing_outside_local_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let settings = ServiceSettings::new(
            "https://github.com/example/plainfeed-data.git".to_owned(),
            Some("secret-token".to_owned()),
        );
        settings.write_to(temporary.path()).unwrap();

        let restored = ServiceSettings::read_from(temporary.path())
            .unwrap()
            .unwrap();
        assert_eq!(restored.remote_url, settings.remote_url);
        assert_eq!(restored.github_token.as_deref(), Some("secret-token"));
        assert!(restored.has_token());
        assert!(settings_path(temporary.path()).starts_with(temporary.path().join(".plainfeed")));
        assert!(
            !temporary
                .path()
                .join(".plainfeed/.service-settings.toml.tmp")
                .exists()
        );
    }

    #[test]
    fn rejects_unknown_versions() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join(".plainfeed")).unwrap();
        fs::write(
            settings_path(temporary.path()),
            "format = \"plainfeed.service-settings/v2\"\nremote_url = \"https://example.com\"\n",
        )
        .unwrap();

        assert!(matches!(
            ServiceSettings::read_from(temporary.path()),
            Err(Error::UnsupportedFormat { .. })
        ));
    }
}
