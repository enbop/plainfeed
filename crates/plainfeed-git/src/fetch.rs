use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
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
    validate_configured_object_format(&request.repository)?;
    let mut repository = match gix::open(&request.repository) {
        Ok(repository) => repository,
        Err(_) if !request.repository.join(".git").exists() => {
            gix::init(&request.repository).map_err(git_error)?
        }
        Err(error) => return Err(git_error(error)),
    };
    request.limits.check_repository(repository.git_dir())?;
    validate_object_format(&repository)?;
    repository
        .committer_or_set_generic_fallback()
        .map_err(git_error)?;

    let source_ref = format!("refs/heads/{}", request.branch);
    let destination_ref = format!("refs/remotes/origin/{}", request.branch);
    // The remote-tracking ref is observational and may follow a force push.
    // The canonical local branch is protected separately by an ancestry check
    // before any live files or refs are activated.
    let refspec = format!("+{source_ref}:{destination_ref}");
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
        .map_err(|error| match error {
            gix::remote::fetch::Error::NoMapping { .. } => Error::MissingRemoteRef {
                name: source_ref.clone(),
            },
            gix::remote::fetch::Error::IncompatibleObjectHash { local, remote } => {
                Error::IncompatibleObjectFormat {
                    local: format!("{local:?}").to_ascii_lowercase(),
                    remote: format!("{remote:?}").to_ascii_lowercase(),
                }
            }
            error => git_error(error),
        })?;

    let reopened = gix::open(repository.git_dir()).map_err(git_error)?;
    let tip = reopened
        .find_reference(destination_ref.as_str())
        .map_err(git_error)?
        .into_fully_peeled_id()
        .map_err(git_error)?;
    let commit = reopened.find_commit(tip).map_err(git_error)?;
    validate_snapshot_objects(&reopened, tip.detach(), request.limits)?;
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

fn validate_snapshot_objects(
    repository: &gix::Repository,
    commit_id: gix::hash::ObjectId,
    limits: FetchLimits,
) -> Result<(), Error> {
    let mut seen = HashSet::new();
    validate_one_object(repository, commit_id, &mut seen, limits, false)?;
    let tree = repository
        .find_commit(commit_id)
        .map_err(git_error)?
        .tree_id()
        .map_err(git_error)?
        .detach();
    validate_one_object(repository, tree, &mut seen, limits, true)
}

fn validate_one_object(
    repository: &gix::Repository,
    id: gix::hash::ObjectId,
    seen: &mut HashSet<gix::hash::ObjectId>,
    limits: FetchLimits,
    recurse_tree: bool,
) -> Result<(), Error> {
    if !seen.insert(id) {
        return Ok(());
    }
    limits.check_object_count(seen.len())?;
    let object = repository.find_object(id).map_err(git_error)?;
    limits.check_object_size(object.data.len())?;
    if !recurse_tree {
        return Ok(());
    }
    if object.kind != gix::objs::Kind::Tree {
        return Err(Error::Git(format!(
            "expected tree {id}, found {:?}",
            object.kind
        )));
    }
    let entries = gix::objs::TreeRefIter::from_bytes(&object.data, id.kind())
        .map(|entry| entry.map(|entry| (entry.mode, entry.oid.to_owned())))
        .collect::<Result<Vec<_>, _>>()
        .map_err(git_error)?;
    for (mode, child) in entries {
        if mode.kind() == EntryKind::Commit {
            return Err(Error::Git(
                "submodules are not supported in fetched trees".to_owned(),
            ));
        }
        validate_one_object(repository, child, seen, limits, mode.is_tree())?;
    }
    Ok(())
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

pub fn worktree_changed_paths(
    repository_path: impl AsRef<Path>,
    commit_oid: &str,
    areas: &[&str],
) -> Result<Vec<String>, Error> {
    let repository_path = repository_path.as_ref();
    let repository = gix::open(repository_path).map_err(git_error)?;
    let tree = find_commit_tree(&repository, commit_oid)?;
    let mut expected = BTreeMap::new();
    collect_tree_entries(&tree, "", &mut expected)?;
    expected.retain(|path, _| area_matches(path, areas));

    let mut actual = BTreeMap::new();
    for area in areas {
        collect_worktree_entries(
            repository_path,
            &repository_path.join(area),
            &mut actual,
            repository.object_hash(),
        )?;
    }

    let all_paths: BTreeSet<_> = expected.keys().chain(actual.keys()).cloned().collect();
    Ok(all_paths
        .into_iter()
        .filter(|path| expected.get(path).map(|(_, oid)| oid) != actual.get(path))
        .collect())
}

fn area_matches(path: &str, areas: &[&str]) -> bool {
    areas
        .iter()
        .any(|area| path == *area || path.starts_with(&format!("{area}/")))
}

fn collect_worktree_entries(
    repository_root: &Path,
    path: &Path,
    entries: &mut BTreeMap<String, gix::hash::ObjectId>,
    hash_kind: gix::hash::Kind,
) -> Result<(), Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    let relative = path.strip_prefix(repository_root).map_err(git_error)?;
    let relative = relative
        .to_str()
        .ok_or_else(|| Error::Git(format!("worktree path is not UTF-8: {}", path.display())))?;
    if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
        entries.insert(relative.to_owned(), gix::hash::ObjectId::null(hash_kind));
        return Ok(());
    }
    if metadata.is_file() {
        let data = fs::read(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        let oid =
            gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &data).map_err(git_error)?;
        entries.insert(relative.to_owned(), oid);
        return Ok(());
    }

    let mut children = fs::read_dir(path)
        .map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        collect_worktree_entries(repository_root, &child.path(), entries, hash_kind)?;
    }
    Ok(())
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
        const CONTENT_GUIDE_PATH: &str = "PLAINFEED-CONTENT-GUIDE.md";
        if let Some(entry) = root_tree.find_entry(CONTENT_GUIDE_PATH) {
            if !matches!(entry.kind(), EntryKind::Blob | EntryKind::BlobExecutable) {
                return Err(Error::Git(format!(
                    "remote {CONTENT_GUIDE_PATH} path is not a file"
                )));
            }
            let blob = entry.object().map_err(git_error)?.into_blob();
            let output = temporary.join(CONTENT_GUIDE_PATH);
            fs::write(&output, &blob.data).map_err(|source| Error::Io {
                path: output,
                source,
            })?;
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

/// Export the complete, already-audited v1 data snapshot for first checkout.
pub fn export_initial_snapshot(
    repository_path: impl AsRef<Path>,
    commit_oid: &str,
    destination: impl AsRef<Path>,
) -> Result<(), Error> {
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let oid = gix::hash::ObjectId::from_hex(commit_oid.as_bytes()).map_err(git_error)?;
    let root_tree = repository
        .find_commit(oid)
        .map_err(git_error)?
        .tree()
        .map_err(git_error)?;
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(Error::Git(format!(
            "initial snapshot destination already exists: {}",
            destination.display()
        )));
    }
    fs::create_dir_all(destination).map_err(|source| Error::Io {
        path: destination.to_owned(),
        source,
    })?;
    let result = export_tree(&root_tree, destination);
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
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

pub fn reference_exists(
    repository_path: impl AsRef<Path>,
    reference_name: &str,
) -> Result<bool, Error> {
    let repository_path = repository_path.as_ref();
    let repository = match gix::open(repository_path) {
        Ok(repository) => repository,
        Err(_) if !repository_path.join(".git").exists() => return Ok(false),
        Err(error) => return Err(git_error(error)),
    };
    repository
        .try_find_reference(reference_name)
        .map(|reference| reference.is_some())
        .map_err(git_error)
}

pub fn set_head_branch(repository_path: impl AsRef<Path>, branch: &str) -> Result<(), Error> {
    validate_branch(branch)?;
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let head = repository.git_dir().join("HEAD");
    let temporary = repository.git_dir().join("HEAD.plainfeed.tmp");
    fs::write(&temporary, format!("ref: refs/heads/{branch}\n")).map_err(|source| Error::Io {
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, &head).map_err(|source| Error::Io { path: head, source })
}

pub fn delete_plainfeed_reference(
    repository_path: impl AsRef<Path>,
    reference_name: &str,
) -> Result<(), Error> {
    if !reference_name.starts_with("refs/plainfeed/") {
        return Err(Error::Git(format!(
            "refusing to delete non-Plainfeed reference {reference_name:?}"
        )));
    }
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    if let Some(reference) = repository
        .try_find_reference(reference_name)
        .map_err(git_error)?
    {
        reference.delete().map_err(git_error)?;
    }
    Ok(())
}

pub fn validate_repository_contract(repository_path: impl AsRef<Path>) -> Result<(), Error> {
    validate_configured_object_format(repository_path.as_ref())?;
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    validate_open_repository_contract(&repository)
}

fn validate_configured_object_format(repository_path: &Path) -> Result<(), Error> {
    let worktree_config = repository_path.join(".git/config");
    let bare_config = repository_path.join("config");
    let config = if worktree_config.is_file() {
        worktree_config
    } else if repository_path.join("HEAD").is_file() && bare_config.is_file() {
        bare_config
    } else {
        return Ok(());
    };
    let text = fs::read_to_string(&config).map_err(|source| Error::Io {
        path: config,
        source,
    })?;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("objectformat")
            && !value.trim().eq_ignore_ascii_case("sha1")
        {
            return Err(Error::UnsupportedObjectFormat {
                actual: value.trim().to_ascii_lowercase(),
            });
        }
    }
    Ok(())
}

fn validate_open_repository_contract(repository: &gix::Repository) -> Result<(), Error> {
    validate_object_format(repository)?;
    let reference_name = "refs/heads/main";
    repository
        .find_reference(reference_name)
        .map_err(|_| Error::MissingLocalRef {
            name: reference_name.to_owned(),
        })?;
    Ok(())
}

fn validate_object_format(repository: &gix::Repository) -> Result<(), Error> {
    if repository.object_hash() != gix::hash::Kind::Sha1 {
        return Err(Error::UnsupportedObjectFormat {
            actual: format!("{:?}", repository.object_hash()).to_ascii_lowercase(),
        });
    }
    Ok(())
}

pub fn is_ancestor(
    repository_path: impl AsRef<Path>,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, Error> {
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    let ancestor = gix::hash::ObjectId::from_hex(ancestor.as_bytes()).map_err(git_error)?;
    let descendant = gix::hash::ObjectId::from_hex(descendant.as_bytes()).map_err(git_error)?;
    if ancestor == descendant {
        return Ok(true);
    }
    let descendant = repository.find_commit(descendant).map_err(git_error)?;
    for commit in descendant.ancestors().all().map_err(git_error)? {
        if commit.map_err(git_error)?.id == ancestor {
            return Ok(true);
        }
    }
    Ok(false)
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

    if let Some(expected_oid) = expected_oid
        && !is_ancestor(
            repository_path.as_ref(),
            &expected_oid.to_string(),
            &remote_oid.to_string(),
        )?
    {
        return Err(Error::NonFastForward {
            base: expected_oid.to_string(),
            remote: remote_oid.to_string(),
        });
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
    use std::{fs, process::Command};

    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    use super::{
        changed_paths, delete_plainfeed_reference, export_initial_snapshot, export_remote_snapshot,
        finalize_fast_forward_checkout, is_ancestor, reference_exists, set_head_branch,
        validate_branch, validate_repository_contract, validate_snapshot_objects,
        worktree_changed_paths,
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
    fn detects_local_changes_only_in_remote_owned_areas() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        let repository = gix::init(&root).unwrap();
        let content = repository.write_blob(b"committed content\n").unwrap();
        let config = repository.write_blob(b"committed config\n").unwrap();
        let state = repository.write_blob(b"committed state\n").unwrap();
        let mut editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        editor
            .upsert("content/item.md", EntryKind::Blob, content)
            .unwrap();
        editor
            .upsert("config/channels.toml", EntryKind::Blob, config)
            .unwrap();
        editor
            .upsert("state/entries/item.toml", EntryKind::Blob, state)
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
        fs::create_dir_all(root.join("state/entries")).unwrap();
        fs::write(root.join("content/item.md"), b"locally changed\n").unwrap();
        fs::write(root.join("content/untracked.md"), b"untracked\n").unwrap();
        fs::write(root.join("state/entries/item.toml"), b"reader-owned\n").unwrap();

        assert_eq!(
            worktree_changed_paths(&root, &commit.to_string(), &["content", "config"],).unwrap(),
            [
                "config/channels.toml",
                "content/item.md",
                "content/untracked.md",
            ]
        );
    }

    #[test]
    fn distinguishes_linear_and_divergent_history() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        let base_tree = repository.empty_tree().id;
        let base = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/main",
                "base",
                base_tree,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();
        let child = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/child",
                "child",
                base_tree,
                [base.detach()],
            )
            .unwrap();
        let sibling = repository
            .commit_as(
                identity,
                identity,
                "refs/heads/sibling",
                "sibling",
                base_tree,
                [base.detach()],
            )
            .unwrap();

        assert!(is_ancestor(repository.path(), &base.to_string(), &child.to_string()).unwrap());
        assert!(!is_ancestor(repository.path(), &child.to_string(), &sibling.to_string()).unwrap());
    }

    #[test]
    fn rejects_missing_main_and_sha256_repositories() {
        let temporary = tempfile::tempdir().unwrap();
        let missing_main = temporary.path().join("missing-main");
        gix::init(&missing_main).unwrap();
        assert!(matches!(
            validate_repository_contract(&missing_main),
            Err(crate::Error::MissingLocalRef { .. })
        ));

        let sha256 = temporary.path().join("sha256");
        let status = Command::new("git")
            .args(["init", "--quiet", "--object-format=sha256"])
            .arg(&sha256)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "native Git must support the SHA-256 fixture"
        );
        assert!(matches!(
            validate_repository_contract(&sha256),
            Err(crate::Error::UnsupportedObjectFormat { .. })
        ));
    }

    #[test]
    fn rejects_snapshot_object_count_and_single_object_overages() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let blob = repository.write_blob(b"sixteen-byte-ish").unwrap();
        let mut editor = repository.edit_tree(repository.empty_tree().id).unwrap();
        editor
            .upsert("content/item.md", EntryKind::Blob, blob)
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
        let mut limits = crate::FetchLimits::default();
        limits.max_object_bytes = 4;
        assert!(matches!(
            validate_snapshot_objects(&repository, commit.detach(), limits),
            Err(crate::Error::ObjectTooLarge { .. })
        ));

        limits.max_object_bytes = usize::MAX;
        limits.max_object_count = 2;
        assert!(matches!(
            validate_snapshot_objects(&repository, commit.detach(), limits),
            Err(crate::Error::TooManyObjects { .. })
        ));
    }

    #[test]
    fn deletes_only_plainfeed_internal_references() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let identity = gix::actor::SignatureRef {
            name: b"Plainfeed Test".as_bstr(),
            email: b"test@plainfeed.invalid".as_bstr(),
            time: "1784160000 +0900",
        };
        repository
            .commit_as(
                identity,
                identity,
                "refs/plainfeed/candidate",
                "candidate",
                repository.empty_tree().id,
                gix::commit::NO_PARENT_IDS,
            )
            .unwrap();

        delete_plainfeed_reference(repository.path(), "refs/plainfeed/candidate").unwrap();
        assert!(
            repository
                .try_find_reference("refs/plainfeed/candidate")
                .unwrap()
                .is_none()
        );
        assert!(delete_plainfeed_reference(repository.path(), "refs/heads/main").is_err());
    }

    #[test]
    fn exports_update_and_initial_snapshots_with_distinct_ownership() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();
        let content = repository.write_blob(b"entry body\n").unwrap();
        let config = repository.write_blob(b"channel config\n").unwrap();
        let state = repository.write_blob(b"must not export\n").unwrap();
        let guide = repository.write_blob(b"producer contract\n").unwrap();
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
        editor
            .upsert("PLAINFEED-CONTENT-GUIDE.md", EntryKind::Blob, guide)
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
        assert_eq!(
            std::fs::read_to_string(staging.join("PLAINFEED-CONTENT-GUIDE.md")).unwrap(),
            "producer contract\n"
        );

        let initial = temporary.path().join("staging/initial");
        export_initial_snapshot(repository.path(), &commit.to_string(), &initial).unwrap();
        assert_eq!(
            std::fs::read_to_string(initial.join("state/entries/private.toml")).unwrap(),
            "must not export\n"
        );
    }

    #[test]
    fn points_an_unborn_checkout_head_at_main() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = gix::init(temporary.path().join("repository")).unwrap();

        assert!(!reference_exists(repository.path(), "refs/heads/main").unwrap());
        set_head_branch(repository.path(), "main").unwrap();
        assert_eq!(
            std::fs::read_to_string(repository.git_dir().join("HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
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
