use std::{fs, path::Path};

use gix::{bstr::ByteSlice, objs::tree::EntryKind};

use crate::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateCommit {
    pub commit: String,
    pub tree: String,
    pub state_tree: String,
    pub changed_paths: Vec<String>,
}

pub fn create_state_commit(
    repository_path: impl AsRef<Path>,
    remote_tip: &str,
    state_directory: impl AsRef<Path>,
    candidate_ref: &str,
    committed_at_unix: i64,
) -> Result<Option<StateCommit>, Error> {
    if !candidate_ref.starts_with("refs/plainfeed/") {
        return Err(Error::Git(format!(
            "state candidate ref must be under refs/plainfeed/: {candidate_ref:?}"
        )));
    }
    gix::refs::FullName::try_from(candidate_ref).map_err(git_error)?;
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let remote_oid = gix::hash::ObjectId::from_hex(remote_tip.as_bytes()).map_err(git_error)?;
    let remote_commit = repository.find_commit(remote_oid).map_err(git_error)?;
    let remote_tree = remote_commit.tree().map_err(git_error)?;
    let mut state_editor = repository
        .edit_tree(repository.empty_tree().id)
        .map_err(git_error)?;
    let mut changed_paths = Vec::new();
    add_state_files(
        &repository,
        state_directory.as_ref(),
        state_directory.as_ref(),
        &mut state_editor,
        &mut changed_paths,
    )?;
    changed_paths.sort_unstable();
    let state_tree = state_editor.write().map_err(git_error)?;
    if remote_tree
        .find_entry("state")
        .is_some_and(|entry| entry.object_id() == state_tree.detach())
    {
        return Ok(None);
    }

    let mut root_editor = repository.edit_tree(remote_tree.id()).map_err(git_error)?;
    let tree = root_editor
        .upsert("state", EntryKind::Tree, state_tree)
        .map_err(git_error)?
        .write()
        .map_err(git_error)?;
    let time = format!("{committed_at_unix} +0000");
    let identity = gix::actor::SignatureRef {
        name: b"Plainfeed".as_bstr(),
        email: b"sync@plainfeed.invalid".as_bstr(),
        time: &time,
    };
    let commit = repository
        .commit_as(
            identity,
            identity,
            candidate_ref,
            "sync(state): publish reader state",
            tree,
            [remote_oid],
        )
        .map_err(git_error)?;

    Ok(Some(StateCommit {
        commit: commit.to_string(),
        tree: tree.to_string(),
        state_tree: state_tree.to_string(),
        changed_paths: changed_paths
            .into_iter()
            .map(|path| format!("state/{path}"))
            .collect(),
    }))
}

fn add_state_files(
    repository: &gix::Repository,
    state_root: &Path,
    directory: &Path,
    editor: &mut gix::object::tree::Editor<'_>,
    changed_paths: &mut Vec<String>,
) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(directory).map_err(|source| Error::Io {
        path: directory.to_owned(),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::Git(format!(
            "state path is not a real directory: {}",
            directory.display()
        )));
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|source| Error::Io {
            path: directory.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| Error::Io {
            path: directory.to_owned(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Git(format!(
                "symbolic links are not allowed in state: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            add_state_files(repository, state_root, &path, editor, changed_paths)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(Error::Git(format!(
                "unsupported state filesystem entry: {}",
                path.display()
            )));
        }
        let relative = path.strip_prefix(state_root).map_err(git_error)?;
        let relative = relative
            .to_str()
            .ok_or_else(|| Error::Git(format!("state path is not UTF-8: {}", path.display())))?;
        let data = fs::read(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let blob = repository.write_blob(&data).map_err(git_error)?;
        editor
            .upsert(relative, EntryKind::Blob, blob)
            .map_err(git_error)?;
        changed_paths.push(relative.to_owned());
    }
    Ok(())
}

fn git_error(error: impl std::fmt::Display + std::fmt::Debug) -> Error {
    Error::Git(format!("{error:#?}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    use super::create_state_commit;

    #[test]
    fn candidate_has_the_remote_parent_and_changes_only_state() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0000",
        };
        let content = repository.write_blob(b"content\n").unwrap();
        let old_state = repository.write_blob(b"favorite = false\n").unwrap();
        let mut editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        editor
            .upsert("content/item.md", EntryKind::Blob, content)
            .unwrap();
        editor
            .upsert("state/entries/item.toml", EntryKind::Blob, old_state)
            .unwrap();
        let tree = editor.write().unwrap();
        let base = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/main",
                "base",
                tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        let state_directory = temporary.path().join("state");
        fs::create_dir_all(state_directory.join("entries")).unwrap();
        fs::write(
            state_directory.join("entries/item.toml"),
            b"favorite = true\n",
        )
        .unwrap();

        let candidate = create_state_commit(
            repository.path(),
            &base.to_string(),
            &state_directory,
            "refs/plainfeed/state-candidate",
            1_784_160_000,
        )
        .unwrap()
        .unwrap();

        let candidate_oid = gix::hash::ObjectId::from_hex(candidate.commit.as_bytes()).unwrap();
        let commit = repository.find_commit(candidate_oid).unwrap();
        assert_eq!(
            commit
                .parent_ids()
                .map(|parent| parent.to_string())
                .collect::<Vec<_>>(),
            [base.to_string()]
        );
        assert_eq!(candidate.changed_paths, ["state/entries/item.toml"]);
        assert_eq!(
            crate::changed_paths(
                repository.path(),
                Some(&base.to_string()),
                &candidate.commit,
            )
            .unwrap(),
            ["state/entries/item.toml"]
        );
    }
}
