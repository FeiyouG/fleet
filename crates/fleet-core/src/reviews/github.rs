//! GitHub adapter via the already-authenticated `gh` CLI.

use super::ReviewAdapter;
use crate::model::{Ci, ReviewRow, ReviewStatus, Threads};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::process::Command;

/// PRs per GraphQL page. Measured on monid-ai/monid (134 open PRs, CodeRabbit on every one):
/// 30 answers in ~2.5 s every time, 50 hits GitHub's gateway timeout (HTTP 502) about half the
/// time, 100 always. `gh pr list --limit 100` (the previous path) therefore returned nothing.
const PAGE: u32 = 30;

/// Everything a `ReviewRow` needs, in one node, with nothing heavy: no review bodies
/// (`reviews`, which drags every CodeRabbit comment along), no `reviewDecision` (3 s per page by
/// itself; `latestReviews` carries the same approvals), no per-check contexts (only the
/// rollup state). Thread counts ride along, so the list needs no per-PR follow-up call.
const PR_FIELDS: &str =
    "number title url updatedAt state isDraft mergeable headRefName baseRefName headRefOid \
author{login} \
latestReviews(first:20){nodes{state author{login}}} \
reviewThreads(first:100){totalCount nodes{isResolved}} \
commits(last:1){nodes{commit{statusCheckRollup{state}}}}";

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

    /// `gh api graphql` with `$owner`/`$name` bound to this repo plus `extra` variables.
    async fn graphql(&self, query: &str, extra: &[(&str, &str)]) -> Result<Value> {
        let (owner, name) = self.owner_name();
        let mut args: Vec<String> = vec![
            "api".into(),
            "graphql".into(),
            "-F".into(),
            format!("owner={owner}"),
            "-F".into(),
            format!("name={name}"),
        ];
        for (k, v) in extra {
            args.push("-F".into());
            args.push(format!("{k}={v}"));
        }
        args.push("-f".into());
        args.push(format!("query={query}"));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = self.gh(&refs).await?;
        if let Some(errs) = v.get("errors").and_then(Value::as_array) {
            if let Some(m) = errs
                .first()
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
            {
                bail!("gh api graphql: {m}");
            }
        }
        Ok(v)
    }

    /// One page of open PRs, newest activity first. `after` is the previous page's cursor.
    async fn page(&self, after: Option<&str>) -> Result<(Vec<ReviewRow>, Option<String>)> {
        let q = format!(
            "query($owner:String!,$name:String!,$n:Int!,$after:String){{repository(owner:$owner,name:$name){{\
             pullRequests(states:OPEN,first:$n,after:$after,orderBy:{{field:UPDATED_AT,direction:DESC}}){{\
             pageInfo{{hasNextPage endCursor}} nodes{{{PR_FIELDS}}}}}}}}}"
        );
        let n = PAGE.to_string();
        let mut vars = vec![("n", n.as_str())];
        if let Some(c) = after {
            vars.push(("after", c));
        }
        let v = self.graphql(&q, &vars).await?;
        let prs = v
            .pointer("/data/repository/pullRequests")
            .context("unexpected graphql shape")?;
        let rows = prs
            .get("nodes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| row_from_node(&self.source, p))
                    .collect()
            })
            .unwrap_or_default();
        let next = prs
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            .then(|| {
                prs.pointer("/pageInfo/endCursor")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .flatten();
        Ok((rows, next))
    }
}

/// `reviewThreads{totalCount nodes{isResolved}}` on a PR node -> counts. GitHub threads are
/// always resolvable.
pub fn threads_of(pr: &Value) -> Threads {
    let Some(rt) = pr.get("reviewThreads") else {
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

/// Pure: one GraphQL `PullRequest` node (see `PR_FIELDS`) -> `ReviewRow`.
pub fn row_from_node(source: &str, pr: &Value) -> Option<ReviewRow> {
    let number = pr.get("number")?.as_u64()?;
    let draft = pr.get("isDraft").and_then(Value::as_bool).unwrap_or(false);
    let state = pr.get("state").and_then(Value::as_str).unwrap_or("OPEN");
    // Combined state of the head commit's checks + statuses; null when there are none.
    let ci = match pr
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(Value::as_str)
    {
        None => Ci::None,
        Some("SUCCESS") => Ci::Passed,
        Some("FAILURE") | Some("ERROR") => Ci::Failed,
        Some("PENDING") | Some("EXPECTED") => Ci::Running,
        Some(_) => Ci::None,
    };
    let mergeable = pr.get("mergeable").and_then(Value::as_str).unwrap_or("");
    // Latest review per reviewer; an APPROVED one is a standing approval.
    let approvals: Vec<String> = pr
        .pointer("/latestReviews/nodes")
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
        _ if !approvals.is_empty() => ReviewStatus::Approved,
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
        threads: threads_of(pr),
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

    /// Every open PR, walking GraphQL pages of `PAGE` until exhausted (sequential: a page's
    /// cursor comes from the previous one). A failing page fails the whole fetch, so the
    /// caller shows the error instead of a silently short list.
    async fn open_reviews(&self) -> Result<Vec<ReviewRow>> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let (rows, next) = self.page(after.as_deref()).await?;
            out.extend(rows);
            match next {
                Some(c) => after = Some(c),
                None => return Ok(out),
            }
        }
    }

    async fn review(&self, id: u64) -> Result<ReviewRow> {
        let q = format!(
            "query($owner:String!,$name:String!,$id:Int!){{repository(owner:$owner,name:$name){{\
             pullRequest(number:$id){{{PR_FIELDS}}}}}}}"
        );
        let id_s = id.to_string();
        let v = self.graphql(&q, &[("id", id_s.as_str())]).await?;
        let node = v
            .pointer("/data/repository/pullRequest")
            .filter(|n| !n.is_null())
            .with_context(|| format!("PR #{id} not found in {}", self.repo))?;
        row_from_node(&self.source, node).context("unexpected PR json")
    }

    async fn merged_for_branch(&self, branch: &str) -> Result<Option<ReviewRow>> {
        let q = format!(
            "query($owner:String!,$name:String!,$branch:String!){{repository(owner:$owner,name:$name){{\
             pullRequests(states:MERGED,headRefName:$branch,first:1,orderBy:{{field:UPDATED_AT,direction:DESC}}){{\
             nodes{{{PR_FIELDS}}}}}}}}}"
        );
        let v = self.graphql(&q, &[("branch", branch)]).await?;
        Ok(v.pointer("/data/repository/pullRequests/nodes")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|p| row_from_node(&self.source, p)))
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

    fn page_nodes() -> Vec<Value> {
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/github-pr-page.json")).unwrap();
        v.pointer("/data/repository/pullRequests/nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap()
    }

    #[test]
    fn parses_page_nodes() {
        let rows: Vec<ReviewRow> = page_nodes()
            .iter()
            .filter_map(|p| row_from_node("github:monid-ai/monid", p))
            .collect();
        assert_eq!(rows.len(), 3);
        // approved by a human (coderabbit only COMMENTED), CI green, 1 of 2 threads open
        let r = &rows[0];
        assert_eq!(r.display_id, "#225");
        assert_eq!(r.approvals, vec!["FeiyouG"]);
        assert_eq!(r.ci, Ci::Passed);
        assert_eq!(r.status, ReviewStatus::Approved);
        assert_eq!(
            (r.threads.total, r.threads.resolved, r.threads.unresolved),
            (2, 1, 1)
        );
        // CI failed beats approval-less open
        let r = &rows[1];
        assert_eq!(r.ci, Ci::Failed);
        assert_eq!(r.status, ReviewStatus::CiFailed);
        assert!(r.approvals.is_empty());
        // draft, no checks at all
        let r = &rows[2];
        assert_eq!(r.ci, Ci::None);
        assert_eq!(r.status, ReviewStatus::Draft);
        assert_eq!(r.threads.total, 0);
    }

    #[test]
    fn page_cursor() {
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/github-pr-page.json")).unwrap();
        let info = v.pointer("/data/repository/pullRequests/pageInfo").unwrap();
        assert_eq!(info["hasNextPage"], Value::Bool(true));
        assert!(info["endCursor"].as_str().unwrap().starts_with("Y3Vyc29y"));
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
