//! File-format parsing and state transitions for Plainfeed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const ENTRY_FORMAT: &str = "plainfeed.entry/v1";
pub const STATE_FORMAT: &str = "plainfeed.state/v1";
pub const CHANNELS_FORMAT: &str = "plainfeed.channels/v1";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct EntryMetadata {
    pub format: String,
    pub id: String,
    pub title: String,
    pub published: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct ChannelFile {
    format: String,
    #[serde(default)]
    channels: Vec<Channel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub metadata: EntryMetadata,
    pub body: String,
    pub path: PathBuf,
    pub state: EntryState,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Comment {
    pub id: String,
    pub created_at: String,
    pub body: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EntryState {
    pub format: String,
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl EntryState {
    pub fn new(entry_id: impl Into<String>) -> Self {
        Self {
            format: STATE_FORMAT.to_owned(),
            entry_id: entry_id.into(),
            read_at: None,
            favorite: false,
            comments: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} does not contain TOML front matter delimited by +++")]
    MissingFrontMatter { path: PathBuf },
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot serialize state: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("unsupported format {actual:?} in {path}; expected {expected:?}")]
    UnsupportedFormat {
        path: PathBuf,
        actual: String,
        expected: &'static str,
    },
    #[error("invalid entry id {id:?} in {path}")]
    InvalidId { path: PathBuf, id: String },
    #[error("invalid channel id {id:?} in {path}")]
    InvalidChannelId { path: PathBuf, id: String },
    #[error("entry id {id:?} does not match file name in {path}")]
    IdPathMismatch { path: PathBuf, id: String },
    #[error("invalid RFC 3339 timestamp {value:?} in {path}")]
    InvalidTimestamp { path: PathBuf, value: String },
    #[error("duplicate entry id {id:?}")]
    DuplicateId { id: String },
    #[error("duplicate channel id {id:?}")]
    DuplicateChannel { id: String },
    #[error("state entry id {actual:?} does not match {expected:?} in {path}")]
    StateIdMismatch {
        path: PathBuf,
        actual: String,
        expected: String,
    },
    #[error("entry {0:?} does not exist")]
    EntryNotFound(String),
    #[error("comment cannot be empty")]
    EmptyComment,
    #[error("state was persisted but its synchronization marker failed: {0}")]
    DirtyMarker(#[from] plainfeed_sync_core::Error),
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> Result<Vec<Entry>, Error> {
        let content_root = self.root.join("content");
        let mut paths = Vec::new();
        collect_markdown_files(&content_root, &mut paths)?;
        paths.sort();

        let mut ids = HashSet::new();
        let mut entries = Vec::with_capacity(paths.len());
        for path in paths {
            let mut entry = parse_entry_file(&path)?;
            if !ids.insert(entry.metadata.id.clone()) {
                return Err(Error::DuplicateId {
                    id: entry.metadata.id,
                });
            }
            entry.state = self.state(&entry.metadata.id)?;
            entries.push(entry);
        }
        entries.sort_by(|left, right| {
            let left_published = OffsetDateTime::parse(&left.metadata.published, &Rfc3339)
                .expect("validated entry timestamp");
            let right_published = OffsetDateTime::parse(&right.metadata.published, &Rfc3339)
                .expect("validated entry timestamp");
            right_published
                .cmp(&left_published)
                .then_with(|| left.metadata.id.cmp(&right.metadata.id))
        });
        Ok(entries)
    }

    pub fn channels(&self) -> Result<Vec<Channel>, Error> {
        let path = self.root.join("config/channels.toml");
        let mut channels = match fs::read_to_string(&path) {
            Ok(text) => {
                let file: ChannelFile = toml::from_str(&text).map_err(|source| Error::Toml {
                    path: path.clone(),
                    source,
                })?;
                if file.format != CHANNELS_FORMAT {
                    return Err(Error::UnsupportedFormat {
                        path,
                        actual: file.format,
                        expected: CHANNELS_FORMAT,
                    });
                }
                file.channels
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(Error::Io { path, source }),
        };

        let mut ids = HashSet::new();
        for channel in &channels {
            validate_channel_id(&channel.id, &path)?;
            if !ids.insert(channel.id.clone()) {
                return Err(Error::DuplicateChannel {
                    id: channel.id.clone(),
                });
            }
        }

        let mut inferred = HashSet::new();
        for entry in self.entries()? {
            for id in entry.metadata.channels {
                if !ids.contains(&id) {
                    inferred.insert(id);
                }
            }
        }
        let mut inferred = inferred.into_iter().collect::<Vec<_>>();
        inferred.sort();
        channels.extend(inferred.into_iter().map(|id| Channel {
            label: channel_fallback_label(&id),
            id,
        }));
        Ok(channels)
    }

    pub fn state(&self, entry_id: &str) -> Result<EntryState, Error> {
        validate_id(entry_id, &self.root)?;
        let path = self.state_path(entry_id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(EntryState::new(entry_id));
            }
            Err(source) => return Err(Error::Io { path, source }),
        };
        let state: EntryState = toml::from_str(&text).map_err(|source| Error::Toml {
            path: path.clone(),
            source,
        })?;
        if state.format != STATE_FORMAT {
            return Err(Error::UnsupportedFormat {
                path,
                actual: state.format,
                expected: STATE_FORMAT,
            });
        }
        if state.entry_id != entry_id {
            return Err(Error::StateIdMismatch {
                path,
                actual: state.entry_id,
                expected: entry_id.to_owned(),
            });
        }
        validate_optional_timestamp(state.read_at.as_deref(), &self.state_path(entry_id))?;
        for comment in &state.comments {
            validate_timestamp(&comment.created_at, &self.state_path(entry_id))?;
        }
        Ok(state)
    }

    pub fn mark_read(&self, entry_id: &str, at: &str) -> Result<EntryState, Error> {
        self.ensure_entry_exists(entry_id)?;
        validate_timestamp(at, &self.root)?;
        let mut state = self.state(entry_id)?;
        if state.read_at.is_none() {
            state.read_at = Some(at.to_owned());
            self.write_state(&state)?;
        }
        Ok(state)
    }

    pub fn set_favorite(&self, entry_id: &str, favorite: bool) -> Result<EntryState, Error> {
        self.ensure_entry_exists(entry_id)?;
        let mut state = self.state(entry_id)?;
        state.favorite = favorite;
        self.write_state(&state)?;
        Ok(state)
    }

    pub fn add_comment(
        &self,
        entry_id: &str,
        comment_id: &str,
        at: &str,
        body: &str,
    ) -> Result<EntryState, Error> {
        self.ensure_entry_exists(entry_id)?;
        validate_timestamp(at, &self.root)?;
        let body = body.trim();
        if body.is_empty() {
            return Err(Error::EmptyComment);
        }
        let mut state = self.state(entry_id)?;
        state.comments.push(Comment {
            id: comment_id.to_owned(),
            created_at: at.to_owned(),
            body: body.to_owned(),
            extra: BTreeMap::new(),
        });
        self.write_state(&state)?;
        Ok(state)
    }

    pub fn write_state(&self, state: &EntryState) -> Result<(), Error> {
        validate_id(&state.entry_id, &self.root)?;
        let directory = self.root.join("state/entries");
        fs::create_dir_all(&directory).map_err(|source| Error::Io {
            path: directory.clone(),
            source,
        })?;
        let destination = self.state_path(&state.entry_id);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = directory.join(format!(".{}.{}.tmp", state.entry_id, nonce));
        let text = toml::to_string_pretty(state)?;
        let result = (|| {
            let mut file = fs::File::create(&temporary).map_err(|source| Error::Io {
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
            let _ = fs::remove_file(&temporary);
        }
        result?;
        plainfeed_sync_core::DirtyJournal::new(&self.root).mark(&state.entry_id)?;
        Ok(())
    }

    fn state_path(&self, entry_id: &str) -> PathBuf {
        self.root
            .join("state/entries")
            .join(format!("{entry_id}.toml"))
    }

    fn ensure_entry_exists(&self, entry_id: &str) -> Result<(), Error> {
        if self
            .entries()?
            .iter()
            .any(|entry| entry.metadata.id == entry_id)
        {
            Ok(())
        } else {
            Err(Error::EntryNotFound(entry_id.to_owned()))
        }
    }
}

pub fn parse_entry_file(path: &Path) -> Result<Entry, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })?;
    parse_entry(path, &text)
}

pub fn parse_entry(path: &Path, text: &str) -> Result<Entry, Error> {
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = normalized.lines();
    if lines.next() != Some("+++") {
        return Err(Error::MissingFrontMatter {
            path: path.to_owned(),
        });
    }
    let mut metadata_lines = Vec::new();
    let mut found_end = false;
    for line in &mut lines {
        if line == "+++" {
            found_end = true;
            break;
        }
        metadata_lines.push(line);
    }
    if !found_end {
        return Err(Error::MissingFrontMatter {
            path: path.to_owned(),
        });
    }
    let metadata_text = metadata_lines.join("\n");
    let metadata: EntryMetadata = toml::from_str(&metadata_text).map_err(|source| Error::Toml {
        path: path.to_owned(),
        source,
    })?;
    validate_metadata(path, &metadata)?;
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    Ok(Entry {
        metadata,
        body,
        path: path.to_owned(),
        state: EntryState::new(""),
    })
}

fn validate_metadata(path: &Path, metadata: &EntryMetadata) -> Result<(), Error> {
    if metadata.format != ENTRY_FORMAT {
        return Err(Error::UnsupportedFormat {
            path: path.to_owned(),
            actual: metadata.format.clone(),
            expected: ENTRY_FORMAT,
        });
    }
    validate_id(&metadata.id, path)?;
    if path.extension().and_then(|value| value.to_str()) == Some("md")
        && path.file_stem().and_then(|value| value.to_str()) != Some(&metadata.id)
    {
        return Err(Error::IdPathMismatch {
            path: path.to_owned(),
            id: metadata.id.clone(),
        });
    }
    validate_timestamp(&metadata.published, path)?;
    for channel in &metadata.channels {
        validate_channel_id(channel, path)?;
    }
    Ok(())
}

fn validate_id(id: &str, path: &Path) -> Result<(), Error> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && id.as_bytes()[0].is_ascii_alphanumeric();
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidId {
            path: path.to_owned(),
            id: id.to_owned(),
        })
    }
}

fn validate_channel_id(id: &str, path: &Path) -> Result<(), Error> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('/')
        && !id.ends_with('/')
        && id.split('/').all(|segment| {
            !segment.is_empty()
                && segment.as_bytes()[0].is_ascii_alphanumeric()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidChannelId {
            path: path.to_owned(),
            id: id.to_owned(),
        })
    }
}

fn channel_fallback_label(id: &str) -> String {
    id.rsplit('/')
        .next()
        .unwrap_or(id)
        .split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_timestamp(value: &str, path: &Path) -> Result<(), Error> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|_| ())
        .map_err(|_| Error::InvalidTimestamp {
            path: path.to_owned(),
            value: value.to_owned(),
        })
}

fn validate_optional_timestamp(value: Option<&str>, path: &Path) -> Result<(), Error> {
    match value {
        Some(value) => validate_timestamp(value, path),
        None => Ok(()),
    }
}

fn collect_markdown_files(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::Io {
                path: directory.to_owned(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_markdown_files(&path, paths)?;
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("md")
        {
            paths.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: &str = r#"+++
format = "plainfeed.entry/v1"
id = "hello-wasi"
title = "Hello, WASI"
published = "2026-07-17T00:00:00Z"
tags = ["rust", "wasi"]
channels = ["technology", "projects/plainfeed"]
source = { name = "Example", url = "https://example.com/hello" }
+++

This is **file-backed** content.
"#;

    #[test]
    fn parses_entry_document() {
        let entry = parse_entry(Path::new("hello-wasi.md"), ENTRY).unwrap();
        assert_eq!(entry.metadata.id, "hello-wasi");
        assert_eq!(entry.metadata.tags, ["rust", "wasi"]);
        assert_eq!(
            entry.metadata.channels,
            ["technology", "projects/plainfeed"]
        );
        assert_eq!(entry.body, "This is **file-backed** content.");
    }

    #[test]
    fn loads_entries_and_persists_state() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("content/2026/07");
        fs::create_dir_all(&content).unwrap();
        fs::write(content.join("hello-wasi.md"), ENTRY).unwrap();
        let store = Store::open(temporary.path());

        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].state.favorite);

        store.set_favorite("hello-wasi", true).unwrap();
        store
            .mark_read("hello-wasi", "2026-07-17T01:00:00Z")
            .unwrap();
        store
            .add_comment(
                "hello-wasi",
                "comment-1",
                "2026-07-17T01:01:00Z",
                "Keep this.",
            )
            .unwrap();

        let state = store.state("hello-wasi").unwrap();
        assert!(state.favorite);
        assert_eq!(state.read_at.as_deref(), Some("2026-07-17T01:00:00Z"));
        assert_eq!(state.comments[0].body, "Keep this.");
    }

    #[test]
    fn successful_state_replacement_creates_a_dirty_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(temporary.path());

        store.write_state(&EntryState::new("hello-wasi")).unwrap();

        let markers = fs::read_dir(temporary.path().join(".plainfeed/dirty"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(markers.len(), 1);
        assert!(
            fs::read_to_string(markers[0].path())
                .unwrap()
                .contains("hello-wasi")
        );
    }

    #[test]
    fn failed_state_replacement_does_not_create_a_dirty_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("state/entries/hello-wasi.toml");
        fs::create_dir_all(&destination).unwrap();
        let store = Store::open(temporary.path());

        assert!(store.write_state(&EntryState::new("hello-wasi")).is_err());
        assert!(!temporary.path().join(".plainfeed/dirty").exists());
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let error = parse_entry(
            Path::new("../escape.md"),
            &ENTRY.replace("hello-wasi", "../escape"),
        )
        .unwrap_err();
        assert!(matches!(error, Error::InvalidId { .. }));
    }

    #[test]
    fn preserves_unknown_state_fields_when_rewriting() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("content");
        let state_directory = temporary.path().join("state/entries");
        fs::create_dir_all(&content).unwrap();
        fs::create_dir_all(&state_directory).unwrap();
        fs::write(content.join("hello-wasi.md"), ENTRY).unwrap();
        fs::write(
            state_directory.join("hello-wasi.toml"),
            r#"format = "plainfeed.state/v1"
entry_id = "hello-wasi"
producer_hint = "keep-me"
"#,
        )
        .unwrap();

        let store = Store::open(temporary.path());
        store.set_favorite("hello-wasi", true).unwrap();
        let rewritten = fs::read_to_string(state_directory.join("hello-wasi.toml")).unwrap();
        assert!(rewritten.contains("producer_hint = \"keep-me\""));
    }

    #[test]
    fn orders_timestamps_by_instant_not_text() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("content");
        fs::create_dir_all(&content).unwrap();
        fs::write(
            content.join("earlier-local-time.md"),
            ENTRY
                .replace("hello-wasi", "earlier-local-time")
                .replace("2026-07-17T00:00:00Z", "2026-07-17T01:00:00+02:00"),
        )
        .unwrap();
        fs::write(
            content.join("later-instant.md"),
            ENTRY
                .replace("hello-wasi", "later-instant")
                .replace("2026-07-17T00:00:00Z", "2026-07-17T00:30:00Z"),
        )
        .unwrap();

        let entries = Store::open(temporary.path()).entries().unwrap();
        assert_eq!(entries[0].metadata.id, "later-instant");
    }

    #[test]
    fn loads_configured_and_inferred_channels() {
        let temporary = tempfile::tempdir().unwrap();
        let content = temporary.path().join("content");
        let config = temporary.path().join("config");
        fs::create_dir_all(&content).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(content.join("hello-wasi.md"), ENTRY).unwrap();
        fs::write(
            config.join("channels.toml"),
            r#"format = "plainfeed.channels/v1"

[[channels]]
id = "technology"
label = "Technology"
"#,
        )
        .unwrap();

        let channels = Store::open(temporary.path()).channels().unwrap();
        assert_eq!(channels[0].label, "Technology");
        assert_eq!(channels[1].id, "projects/plainfeed");
        assert_eq!(channels[1].label, "Plainfeed");
    }
}
