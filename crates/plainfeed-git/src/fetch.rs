use std::{path::PathBuf, sync::atomic::AtomicBool};

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

fn git_error(error: impl std::fmt::Display) -> Error {
    Error::Git(error.to_string())
}
