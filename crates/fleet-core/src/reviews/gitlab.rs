//! GitLab adapter via `glab api` (already authenticated; fleet never handles tokens).

use super::ReviewAdapter;
use crate::model::{Ci, ReviewRow, ReviewStatus, Threads};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::process::Command;

pub struct GitLab {
    /// `group/project`
    pub project: String,
    source: String,
}

impl GitLab {
    pub fn new(project: &str) -> Self {
        Self {
            project: project.to_string(),
            source: format!("gitlab:{project}"),
        }
    }

    fn enc(&self) -> String {
        self.project.replace('/', "%2F")
    }

    async fn api(&self, path: &str) -> Result<Value> {
        let out = Command::new("glab")
            .args(["api", path])
            .output()
            .await
            .context("running glab")?;
        if !out.status.success() {
            bail!(
                "glab api {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        serde_json::from_slice(&out.stdout).with_context(|| format!("parsing glab api {path}"))
    }

    async fn approvals(&self, iid: u64) -> Vec<String> {
        let v = self
            .api(&format!(
                "projects/{}/merge_requests/{iid}/approvals",
                self.enc()
            ))
            .await;
        v.ok()
            .and_then(|v| v.get("approved_by").cloned())
            .and_then(|a| {
                a.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|x| {
                            x.pointer("/user/username")
                                .and_then(Value::as_str)
                                .map(String::from)
                        })
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    async fn threads(&self, iid: u64) -> Threads {
        let v = self
            .api(&format!(
                "projects/{}/merge_requests/{iid}/discussions?per_page=100",
                self.enc()
            ))
            .await;
        v.ok().map(|v| threads_from(&v)).unwrap_or_default()
    }

    async fn enrich(&self, mut row: ReviewRow) -> ReviewRow {
        if !row.status.is_done() {
            let (a, t) = tokio::join!(self.approvals(row.id), self.threads(row.id));
            row.approvals = a;
            row.threads = t;
            if !row.approvals.is_empty() && row.status == ReviewStatus::Open {
                row.status = ReviewStatus::Approved;
            }
        }
        row
    }
}

/// Count user-visible threads (non-system first note) and resolvable ones.
pub fn threads_from(discussions: &Value) -> Threads {
    let mut t = Threads::default();
    for d in discussions.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(first) = d
            .get("notes")
            .and_then(Value::as_array)
            .and_then(|n| n.first())
        else {
            continue;
        };
        if first
            .get("system")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        t.total += 1;
        if first
            .get("resolvable")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if first
                .get("resolved")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                t.resolved += 1;
            } else {
                t.unresolved += 1;
            }
        }
    }
    t
}

/// Pure: an MR JSON object -> `ReviewRow` (approvals/threads filled later).
pub fn row_from_mr(source: &str, mr: &Value) -> Option<ReviewRow> {
    let iid = mr.get("iid")?.as_u64()?;
    let state = mr.get("state").and_then(Value::as_str).unwrap_or("opened");
    let draft = mr.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let ci = match mr.pointer("/head_pipeline/status").and_then(Value::as_str) {
        Some("success") => Ci::Passed,
        Some("failed") | Some("canceled") => Ci::Failed,
        Some("running")
        | Some("pending")
        | Some("created")
        | Some("preparing")
        | Some("waiting_for_resource") => Ci::Running,
        _ => Ci::None,
    };
    let dms = mr
        .get("detailed_merge_status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let conflicts = mr
        .get("has_conflicts")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || dms == "conflict";
    let status = match state {
        "merged" => ReviewStatus::Merged,
        "closed" | "locked" => ReviewStatus::Closed,
        _ if draft => ReviewStatus::Draft,
        _ if conflicts => ReviewStatus::Conflicts,
        _ if ci == Ci::Failed || dms == "ci_must_pass" => ReviewStatus::CiFailed,
        _ if dms == "mergeable" => ReviewStatus::Mergeable,
        _ => ReviewStatus::Open,
    };
    Some(ReviewRow {
        source: source.to_string(),
        id: iid,
        display_id: format!("!{iid}"),
        title: mr
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        author: mr
            .pointer("/author/username")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        source_branch: mr
            .get("source_branch")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        target_branch: mr
            .get("target_branch")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        draft,
        ci,
        approvals: vec![],
        threads: Threads::default(),
        status,
        head_sha: mr
            .get("sha")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: mr
            .get("web_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        updated_at: mr
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    })
}

#[async_trait]
impl ReviewAdapter for GitLab {
    fn source(&self) -> &str {
        &self.source
    }

    async fn open_reviews(&self) -> Result<Vec<ReviewRow>> {
        // Offset pages of 100 until a short page: a project with more than 100 open MRs must
        // not silently show the newest 100 only.
        let mut base: Vec<ReviewRow> = vec![];
        for page in 1.. {
            let v = self
                .api(&format!(
                    "projects/{}/merge_requests?state=opened&per_page=100&page={page}&order_by=updated_at",
                    self.enc()
                ))
                .await?;
            let got = v.as_array().map(Vec::len).unwrap_or(0);
            base.extend(
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| row_from_mr(&self.source, m)),
            );
            if got < 100 {
                break;
            }
        }
        // Enrich concurrently (one /approvals + one /discussions per MR).
        let futs: Vec<_> = base.into_iter().map(|r| self.enrich(r)).collect();
        Ok(futures_join_all(futs).await)
    }

    async fn review(&self, id: u64) -> Result<ReviewRow> {
        let v = self
            .api(&format!("projects/{}/merge_requests/{id}", self.enc()))
            .await?;
        let row = row_from_mr(&self.source, &v).context("unexpected MR json")?;
        Ok(self.enrich(row).await)
    }

    async fn merged_for_branch(&self, branch: &str) -> Result<Option<ReviewRow>> {
        let v = self
            .api(&format!(
                "projects/{}/merge_requests?state=merged&source_branch={}&order_by=updated_at&per_page=1",
                self.enc(),
                urlenc(branch)
            ))
            .await?;
        Ok(v.as_array()
            .and_then(|a| a.first())
            .and_then(|m| row_from_mr(&self.source, m)))
    }

    fn fetch_refspec(&self, id: u64) -> String {
        format!("merge-requests/{id}/head")
    }

    fn parse_id(&self, s: &str) -> Option<u64> {
        let s = s.trim();
        if let Some(n) = s
            .strip_prefix('!')
            .or(Some(s))
            .and_then(|x| x.parse::<u64>().ok())
        {
            return Some(n);
        }
        // https://gitlab.com/group/project/-/merge_requests/514
        if s.contains("/merge_requests/") && s.contains(&self.project) {
            return s
                .rsplit('/')
                .next()
                .and_then(|x| x.split(['?', '#']).next())
                .and_then(|x| x.parse().ok());
        }
        None
    }

    fn id_prefix(&self) -> &'static str {
        "!"
    }
}

/// Run the enrich futures with bounded concurrency (16 at a time) to be kind to the API.
async fn futures_join_all<F: std::future::Future<Output = ReviewRow>>(
    futs: Vec<F>,
) -> Vec<ReviewRow> {
    let mut out = Vec::with_capacity(futs.len());
    let mut futs = futs.into_iter().peekable();
    while futs.peek().is_some() {
        let chunk: Vec<F> = futs.by_ref().take(16).collect();
        out.extend(futures::future::join_all(chunk).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mr() {
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/gitlab-mr.json")).unwrap();
        let r = row_from_mr("gitlab:monid-ai/monid-services", &v).unwrap();
        assert_eq!(r.id, 541);
        assert_eq!(r.display_id, "!541");
        assert_eq!(r.source_branch, "fix/exa-agent-no-stop");
        assert_eq!(r.ci, Ci::Running);
        assert_eq!(r.status, ReviewStatus::Open);
        assert_eq!(r.author, "hongtao4");
    }

    #[test]
    fn parses_threads() {
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/gitlab-discussions.json"))
                .unwrap();
        let t = threads_from(&v);
        assert!(t.total >= t.resolved + t.unresolved);
        assert_eq!(t.resolved, 3);
        assert_eq!(t.unresolved, 1);
    }

    #[test]
    fn id_parse() {
        let g = GitLab::new("monid-ai/monid-services");
        assert_eq!(g.parse_id("514"), Some(514));
        assert_eq!(g.parse_id("!514"), Some(514));
        assert_eq!(
            g.parse_id("https://gitlab.com/monid-ai/monid-services/-/merge_requests/514"),
            Some(514)
        );
        assert_eq!(
            g.parse_id("https://gitlab.com/other/repo/-/merge_requests/514"),
            None
        );
    }
}

/// Percent-encode a query value (branch names carry `/`, `#`, `+`, …).
fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod urlenc_tests {
    #[test]
    fn encodes_branch_names() {
        assert_eq!(
            super::urlenc("refactor/internal-api-deno"),
            "refactor%2Finternal-api-deno"
        );
        assert_eq!(super::urlenc("a+b#c"), "a%2Bb%23c");
    }
}
