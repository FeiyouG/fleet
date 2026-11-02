//! git worktrees of a repo, with sync state.

use crate::config::Project;
use crate::model::WorktreeRow;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub async fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn git_ok(cwd: &Path, args: &[&str]) -> Option<String> {
    git(cwd, args).await.ok().map(|s| s.trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Porcelain {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain`.
pub fn parse_porcelain(s: &str) -> Vec<Porcelain> {
    let mut out = vec![];
    let mut cur: Option<Porcelain> = None;
    for line in s.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(Porcelain {
                path: PathBuf::from(p),
                head: String::new(),
                branch: None,
                detached: false,
                bare: false,
                prunable: false,
            });
        } else if let Some(c) = cur.as_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                c.head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch ") {
                c.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "detached" {
                c.detached = true;
            } else if line == "bare" {
                c.bare = true;
            } else if line.starts_with("prunable") {
                c.prunable = true;
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// All worktrees of a configured repo, with sync state. Missing folders are reported, not skipped.
pub async fn worktrees(repo: &Project) -> Result<Vec<WorktreeRow>> {
    let listing = git(&repo.repo, &["worktree", "list", "--porcelain"]).await?;
    let entries = parse_porcelain(&listing);
    let main_path = repo
        .repo
        .canonicalize()
        .unwrap_or_else(|_| repo.repo.clone());

    let futs = entries
        .into_iter()
        .filter(|e| !e.bare)
        .map(|e| row_for(repo, &main_path, e));
    Ok(futures::future::join_all(futs).await)
}

async fn row_for(repo: &Project, main_path: &Path, e: Porcelain) -> WorktreeRow {
    {
        let missing = e.prunable || !e.path.exists();
        let path = if missing {
            e.path.clone()
        } else {
            e.path.canonicalize().unwrap_or(e.path.clone())
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut row = WorktreeRow {
            repo: repo.name.clone(),
            is_main: path == *main_path,
            path: path.clone(),
            name,
            branch: e.branch,
            head: e.head.chars().take(8).collect(),
            upstream: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            detached: e.detached,
            missing,
        };
        if !missing {
            let dirty_f = git_ok(
                &path,
                &["status", "--porcelain", "--untracked-files=normal"],
            );
            let upstream_f = async {
                if e.detached {
                    return None;
                }
                git_ok(
                    &path,
                    &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
                )
                .await
            };
            let (dirty, upstream) = tokio::join!(dirty_f, upstream_f);
            row.dirty = dirty.map(|s| !s.is_empty()).unwrap_or(false);
            row.upstream = upstream;
            if row.upstream.is_some() {
                if let Some(s) = git_ok(
                    &path,
                    &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
                )
                .await
                {
                    let mut it = s.split_whitespace();
                    row.behind = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    row.ahead = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                }
            }
        }
        row
    }
}

pub async fn fetch(repo: &Path, remote: &str, refspec: &str) -> Result<()> {
    git(repo, &["fetch", "--quiet", remote, refspec])
        .await
        .map(|_| ())
}

pub async fn worktree_add_branch(
    repo: &Path,
    path: &Path,
    branch: &str,
    start: &str,
) -> Result<()> {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            &path.to_string_lossy(),
            start,
        ],
    )
    .await
    .map(|_| ())
}

pub async fn worktree_add_detached(repo: &Path, path: &Path, rev: &str) -> Result<()> {
    git(
        repo,
        &["worktree", "add", "--detach", &path.to_string_lossy(), rev],
    )
    .await
    .map(|_| ())
}

pub async fn worktree_remove(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let p = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&p);
    git(repo, &args).await.map(|_| ())
}

pub async fn worktree_move(repo: &Path, from: &Path, to: &Path) -> Result<()> {
    git(
        repo,
        &[
            "worktree",
            "move",
            &from.to_string_lossy(),
            &to.to_string_lossy(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn worktree_prune(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"]).await.map(|_| ())
}

pub async fn branch_rename(repo: &Path, old: &str, new: &str) -> Result<()> {
    git(repo, &["branch", "-m", old, new]).await.map(|_| ())
}

/// `-d` (safe delete: refuses unmerged).
pub async fn branch_delete(repo: &Path, branch: &str) -> Result<()> {
    git(repo, &["branch", "-d", branch]).await.map(|_| ())
}

pub async fn reset_hard(wt: &Path, rev: &str) -> Result<()> {
    git(wt, &["reset", "--hard", rev]).await.map(|_| ())
}

pub async fn push_set_upstream(wt: &Path, branch: &str) -> Result<()> {
    git(wt, &["push", "-u", "origin", branch]).await.map(|_| ())
}

pub async fn pull_ff(wt: &Path) -> Result<()> {
    git(wt, &["pull", "--ff-only"]).await.map(|_| ())
}

pub async fn toplevel(cwd: &Path) -> Option<PathBuf> {
    git_ok(cwd, &["rev-parse", "--show-toplevel"])
        .await
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parse() {
        let s = "worktree /a/main\nHEAD 23ec8a20aaaa\nbranch refs/heads/main\n\nworktree /tmp/wt-head\nHEAD a6013d13bbbb\ndetached\n\nworktree /a/x\nHEAD 1111\nbranch refs/heads/fix/x\nprunable gitdir file points to non-existent location\n";
        let v = parse_porcelain(s);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].branch.as_deref(), Some("main"));
        assert!(v[1].detached && v[1].branch.is_none());
        assert!(v[2].prunable);
    }
}
