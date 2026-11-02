//! GitHub adapter via the already-authenticated `gh` CLI.

use super::ReviewAdapter;
use crate::model::{Ci, ReviewRow, ReviewStatus, Threads};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::process::Command;

const FIELDS: &str = "number,title,author,headRefName,baseRefName,isDraft,mergeable,reviewDecision,statusCheckRollup,headRefOid,url,updatedAt,reviews";

pub struct GitHub {
    /// `owner/repo`
    pub repo: String,
    source: String,
}

impl GitHub {
    pub fn new(repo: &str) -> Self {
        Self {
            repo: repo.to_string(),
            source: format!("github:{repo}"),
        }
    }

    fn owner_name(&self) -> (&str, &str) {
        self.repo.split_once('/').unwrap_or((&self.repo, ""))
    }

    async fn gh(&self, args: &[&str]) -> Result<Value> {
        let out = Command::new("gh")
            .args(args)
            .output()
            .await
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "gh {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        serde_json::from_slice(&out.stdout)
            .with_context(|| format!("parsing gh {}", args.join(" ")))
    }

    async fn threads(&self, number: u64) -> Threads {
        let (owner, name) = self.owner_name();
        let q = format!(
            "query {{ repository(owner:\"{owner}\", name:\"{name}\") {{ pullRequest(number:{number}) {{ reviewThreads(first:100) {{ totalCount nodes {{ isResolved }} }} }} }} }}"
        );
        self.gh(&["api", "graphql", "-f", &format!("query={q}")])
            .await
            .map(|v| threads_from(&v))
            .unwrap_or_default()
    }

    async fn enrich(&self, mut row: ReviewRow) -> ReviewRow {
        if !row.status.is_done() {
            row.threads = self.threads(row.id).await;
        }
        row
    }
}

/// `reviewThreads` GraphQL payload -> counts. GitHub threads are always resolvable.
pub fn threads_from(v: &Value) -> Threads {
    let Some(rt) = v.pointer("/data/repository/pullRequest/reviewThreads") else {
        return Threads::default();
    };
    let nodes = rt
        .get("nodes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let resolved = nodes
        .iter()
        .filter(|n| {
            n.get("isResolved")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count() as u32;
    let total = rt
        .get("totalCount")
        .and_then(Value::as_u64)
        .unwrap_or(nodes.len() as u64) as u32;
    Threads {
        total,
        resolved,
        unresolved: total.saturating_sub(resolved),
    }
}

/// Pure: one `gh pr list --json` object -> `ReviewRow` (threads filled later).
pub fn row_from_pr(source: &str, pr: &Value) -> Option<ReviewRow> {
    let number = pr.get("number")?.as_u64()?;
    let draft = pr.get("isDraft").and_then(Value::as_bool).unwrap_or(false);
    let state = pr.get("state").and_then(Value::as_str).unwrap_or("OPEN");
    let checks = pr
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let ci = if checks.is_empty() {
        Ci::None
    } else if checks.iter().any(|c| {
        matches!(
            c.get("conclusion").and_then(Value::as_str),
            Some("FAILURE") | Some("TIMED_OUT") | Some("CANCELLED") | Some("ACTION_REQUIRED")
        ) || c.get("state").and_then(Value::as_str) == Some("FAILURE")
    }) {
        Ci::Failed
    } else if checks.iter().any(|c| {
        matches!(
            c.get("status").and_then(Value::as_str),
            Some("IN_PROGRESS") | Some("QUEUED") | Some("PENDING")
        ) || c.get("state").and_then(Value::as_str) == Some("PENDING")
    }) {
        Ci::Running
    } else {
        Ci::Passed
    };
    let mergeable = pr.get("mergeable").and_then(Value::as_str).unwrap_or("");
    let decision = pr
        .get("reviewDecision")
        .and_then(Value::as_str)
        .unwrap_or("");
    let approvals: Vec<String> = pr
        .get("reviews")
        .and_then(Value::as_array)
        .map(|rs| {
            let mut v: Vec<String> = rs
                .iter()
                .filter(|r| r.get("state").and_then(Value::as_str) == Some("APPROVED"))
                .filter_map(|r| {
                    r.pointer("/author/login")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default();
    let status = match state {
        "MERGED" => ReviewStatus::Merged,
        "CLOSED" => ReviewStatus::Closed,
        _ if draft => ReviewStatus::Draft,
        _ if mergeable == "CONFLICTING" => ReviewStatus::Conflicts,
        _ if ci == Ci::Failed => ReviewStatus::CiFailed,
        _ if decision == "APPROVED" || !approvals.is_empty() => ReviewStatus::Approved,
        _ => ReviewStatus::Open,
    };
    Some(ReviewRow {
        source: source.to_string(),
        id: number,
        display_id: format!("#{number}"),
        title: pr
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        author: pr
            .pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        source_branch: pr
            .get("headRefName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        target_branch: pr
            .get("baseRefName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        draft,
        ci,
        approvals,
        threads: Threads::default(),
        status,
        head_sha: pr
            .get("headRefOid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: pr
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        updated_at: pr
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    })
}

#[async_trait]
impl ReviewAdapter for GitHub {
    fn source(&self) -> &str {
        &self.source
    }

    async fn open_reviews(&self) -> Result<Vec<ReviewRow>> {
        let v = self
            .gh(&[
                "pr", "list", "-R", &self.repo, "--state", "open", "--limit", "100", "--json",
                FIELDS,
            ])
            .await?;
        let base: Vec<ReviewRow> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| row_from_pr(&self.source, p))
                    .collect()
            })
            .unwrap_or_default();
        let mut out = Vec::with_capacity(base.len());
        let mut it = base.into_iter().peekable();
        while it.peek().is_some() {
            let chunk: Vec<_> = it.by_ref().take(8).map(|r| self.enrich(r)).collect();
            out.extend(futures::future::join_all(chunk).await);
        }
        Ok(out)
    }

    async fn review(&self, id: u64) -> Result<ReviewRow> {
        let v = self
            .gh(&[
                "pr",
                "view",
                &id.to_string(),
                "-R",
                &self.repo,
                "--json",
                &format!("{FIELDS},state"),
            ])
            .await?;
        let row = row_from_pr(&self.source, &v).context("unexpected PR json")?;
        Ok(self.enrich(row).await)
    }

    async fn merged_for_branch(&self, branch: &str) -> Result<Option<ReviewRow>> {
        let v = self
            .gh(&[
                "pr",
                "list",
                "-R",
                &self.repo,
                "--head",
                branch,
                "--state",
                "merged",
                "--limit",
                "1",
                "--json",
                "number,title,author,headRefName,baseRefName,headRefOid,url,updatedAt,state",
            ])
            .await?;
        Ok(v.as_array()
            .and_then(|a| a.first())
            .and_then(|p| row_from_pr(&self.source, p)))
    }

    fn fetch_refspec(&self, id: u64) -> String {
        format!("pull/{id}/head")
    }

    fn parse_id(&self, s: &str) -> Option<u64> {
        let s = s.trim();
        if let Some(n) = s
            .strip_prefix('#')
            .or(Some(s))
            .and_then(|x| x.parse::<u64>().ok())
        {
            return Some(n);
        }
        if s.contains("/pull/") && s.contains(&self.repo) {
            return s
                .rsplit('/')
                .next()
                .and_then(|x| x.split(['?', '#']).next())
                .and_then(|x| x.parse().ok());
        }
        None
    }

    fn id_prefix(&self) -> &'static str {
        "#"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prs() {
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/github-prs.json")).unwrap();
        let rows: Vec<ReviewRow> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| row_from_pr("github:monid-ai/cli", p))
            .collect();
        assert_eq!(rows.len(), 3);
        let r = rows.iter().find(|r| r.id == 27).unwrap();
        assert_eq!(r.display_id, "#27");
        assert_eq!(r.author, "FeiyouG");
        assert_eq!(r.source_branch, "feat/rate-limit-errors");
        assert_eq!(r.ci, Ci::Passed);
        assert_eq!(r.status, ReviewStatus::Open);
        assert!(r.approvals.is_empty()); // coderabbit only COMMENTED
    }

    #[test]
    fn parses_threads() {
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/github-threads.json")).unwrap();
        let t = threads_from(&v);
        assert_eq!((t.total, t.resolved, t.unresolved), (2, 0, 2));
    }

    #[test]
    fn id_parse() {
        let g = GitHub::new("monid-ai/cli");
        assert_eq!(g.parse_id("27"), Some(27));
        assert_eq!(g.parse_id("#27"), Some(27));
        assert_eq!(
            g.parse_id("https://github.com/monid-ai/cli/pull/27"),
            Some(27)
        );
        assert_eq!(g.parse_id("https://github.com/other/repo/pull/27"), None);
    }
}
