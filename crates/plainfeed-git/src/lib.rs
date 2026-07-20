//! Plainfeed policy on top of the generic `git-wasip2` transport and repository API.

use std::path::{Path, PathBuf};

pub use git_wasip2::{
    Credentials, Error, FetchLimits, PushOutcome, Remote, changed_paths, commit_root_entry_oid,
    is_ancestor, push_one_commit, reference_exists, reference_oid, set_head_branch,
    worktree_changed_paths,
};

mod state_commit;

pub use state_commit::{StateCommit, create_state_commit};

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
    let repository = request.repository.clone();
    let outcome = git_wasip2::fetch(git_wasip2::FetchRequest {
        repository: request.repository,
        remote: request.remote,
        remote_name: "origin".to_owned(),
        branch: request.branch,
        limits: request.limits,
    })
    .await?;
    let state_tree = commit_root_entry_oid(&repository, &outcome.remote_tip, "state")?;
    Ok(FetchOutcome {
        remote_tip: outcome.remote_tip,
        state_tree,
        remote_refs: outcome.remote_refs,
        repository_bytes: outcome.repository_bytes,
    })
}

pub fn export_remote_snapshot(
    repository_path: impl AsRef<Path>,
    commit_oid: &str,
    destination: impl AsRef<Path>,
) -> Result<(), Error> {
    git_wasip2::export_selected_snapshot(
        repository_path,
        commit_oid,
        destination,
        &["content", "config", "PLAINFEED-CONTENT-GUIDE.md"],
    )
}

pub fn export_initial_snapshot(
    repository_path: impl AsRef<Path>,
    commit_oid: &str,
    destination: impl AsRef<Path>,
) -> Result<(), Error> {
    git_wasip2::export_full_snapshot(repository_path, commit_oid, destination)
}

pub fn delete_plainfeed_reference(
    repository_path: impl AsRef<Path>,
    reference_name: &str,
) -> Result<(), Error> {
    git_wasip2::delete_reference_under(repository_path, reference_name, "refs/plainfeed/")
}

pub fn validate_repository_contract(repository_path: impl AsRef<Path>) -> Result<(), Error> {
    git_wasip2::validate_repository(repository_path, Some("main"))
}

pub fn finalize_fast_forward_checkout(
    repository_path: impl AsRef<Path>,
    branch: &str,
    expected_previous: Option<&str>,
    remote_tip: &str,
) -> Result<(), Error> {
    git_wasip2::finalize_fast_forward_checkout(
        repository_path,
        branch,
        expected_previous,
        remote_tip,
        "plainfeed: activate synchronized snapshot",
    )
}
