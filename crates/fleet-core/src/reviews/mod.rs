//! The reviews collector family. One adapter instance per configured source
//! (`gitlab:group/project`, `github:owner/repo`).

pub mod github;
pub mod gitlab;

use crate::model::ReviewRow;
use anyhow::{bail, Result};
use async_trait::async_trait;

#[async_trait]
pub trait ReviewAdapter: Send + Sync {
    /// `gitlab:group/project`
    fn source(&self) -> &str;

    /// Every open review of the source.
    async fn open_reviews(&self) -> Result<Vec<ReviewRow>>;

    /// One review by id, any state.
    async fn review(&self, id: u64) -> Result<ReviewRow>;

    /// The most recently merged review whose source branch is `branch`, if any. Not enriched
    /// (no approvals/threads): callers only need its status, id and head sha. The open-review
    /// list never contains merged ones, so `close` and gc ask for them per branch.
    async fn merged_for_branch(&self, branch: &str) -> Result<Option<ReviewRow>>;

    /// The refspec that fetches the review's head into `FETCH_HEAD`.
    fn fetch_refspec(&self, id: u64) -> String;

    /// Parse an id or URL belonging to this source.
    fn parse_id(&self, s: &str) -> Option<u64>;

    /// The forge's id prefix (`!` GitLab, `#` GitHub).
    fn id_prefix(&self) -> &'static str;
}

pub fn adapter_for(source: &str) -> Result<Box<dyn ReviewAdapter>> {
    match source.split_once(':') {
        Some(("gitlab", project)) => Ok(Box::new(gitlab::GitLab::new(project))),
        Some(("github", repo)) => Ok(Box::new(github::GitHub::new(repo))),
        Some((kind, _)) => {
            bail!("unknown review source kind `{kind}` in `{source}` (known: gitlab, github)")
        }
        None => bail!("review source must look like `gitlab:group/project`, got `{source}`"),
    }
}
