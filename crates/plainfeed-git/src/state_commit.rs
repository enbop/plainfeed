use std::path::Path;

use git_wasip2::{DirectoryCommitRequest, create_directory_commit};

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
    let candidate = create_directory_commit(DirectoryCommitRequest {
        repository: repository_path.as_ref().to_owned(),
        parent: remote_tip.to_owned(),
        source_directory: state_directory.as_ref().to_owned(),
        root_entry: "state".to_owned(),
        candidate_ref: candidate_ref.to_owned(),
        committed_at_unix,
        author_name: "Plainfeed".to_owned(),
        author_email: "sync@plainfeed.invalid".to_owned(),
        message: "sync(state): publish reader state".to_owned(),
    })?;
    Ok(candidate.map(|candidate| StateCommit {
        commit: candidate.commit,
        tree: candidate.tree,
        state_tree: candidate.entry_tree,
        changed_paths: candidate.changed_paths,
    }))
}
