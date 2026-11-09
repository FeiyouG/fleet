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
    /// `Some(reason)` (possibly empty) when `git worktree lock` was used.
    pub locked: Option<String>,
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
                locked: None,
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
            } else if line == "locked" {
                c.locked = Some(String::new());
            } else if let Some(r) = line.strip_prefix("locked ") {
                c.locked = Some(r.to_string());
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
            machine: None,
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
            in_base: false,
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
            let in_base_f = async {
                if e.detached || path == *main_path || e.head.is_empty() {
                    return false;
                }
                let mut anchors = vec![format!("refs/heads/{}", repo.base)];
                if let Some(remotes) = git_ok(&repo.repo, &["remote"]).await {
                    anchors.extend(
                        remotes
                            .lines()
                            .filter(|r| !r.is_empty())
                            .map(|r| format!("refs/remotes/{r}/{}", repo.base)),
                    );
                }
                for a in anchors {
                    if is_ancestor(&repo.repo, &e.head, &a).await {
                        return true;
                    }
                }
                false
            };
            let (dirty, upstream, in_base) = tokio::join!(dirty_f, upstream_f, in_base_f);
            row.in_base = in_base;
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

/// The URL of `remote` (`git remote get-url`).
pub async fn remote_url(repo: &Path, remote: &str) -> Result<String> {
    Ok(git(repo, &["remote", "get-url", remote])
        .await?
        .trim()
        .to_string())
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

/// `git worktree remove` with `force` = 0 (refuses dirty), 1 (`-f`: drops changes),
/// 2 (`-f -f`: also a locked worktree).
pub async fn worktree_remove(repo: &Path, path: &Path, force: u8) -> Result<()> {
    let p = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    args.extend(std::iter::repeat_n("--force", force.min(2) as usize));
    args.push(&p);
    git(repo, &args).await.map(|_| ())
}

/// `git worktree add <path> <branch>` for a branch that already exists.
pub async fn worktree_add_existing(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    git(repo, &["worktree", "add", &path.to_string_lossy(), branch])
        .await
        .map(|_| ())
}

/// `git worktree list --porcelain` of a repo, parsed.
pub async fn porcelain(repo: &Path) -> Result<Vec<Porcelain>> {
    Ok(parse_porcelain(
        &git(repo, &["worktree", "list", "--porcelain"]).await?,
    ))
}

/// The main clone of the repo that `path` (any worktree, any subdirectory) belongs to.
pub async fn main_clone(path: &Path) -> Option<PathBuf> {
    let common = git_ok(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let common = PathBuf::from(common);
    // `<main>/.git` for a normal clone; a bare repo is its own "main".
    if common.file_name().map(|n| n == ".git").unwrap_or(false) {
        common.parent().map(Path::to_path_buf)
    } else {
        Some(common)
    }
}

/// `git status --porcelain` lines (modified + untracked), empty when clean.
pub async fn status_lines(wt: &Path) -> Result<Vec<String>> {
    Ok(
        git(wt, &["status", "--porcelain", "--untracked-files=normal"])
            .await?
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// Branch tips that exist on the remotes *right now* (`git ls-remote --heads` of every
/// remote), restricted to commits present locally (others cannot be reachability anchors).
/// Local remote-tracking refs are not trusted: after a forge deletes a merged branch, a
/// stale `origin/<branch>` would make its commits look safe until the next `fetch --prune`.
/// `Err` when a remote cannot be reached.
pub async fn live_remote_tips(repo: &Path) -> Result<Vec<String>> {
    let remotes = git(repo, &["remote"]).await?;
    let mut shas = vec![];
    for r in remotes.lines().filter(|l| !l.is_empty()) {
        let fut = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["ls-remote", "--heads", r])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(std::time::Duration::from_secs(20), fut)
            .await
            .with_context(|| format!("git ls-remote {r}: timed out"))?
            .context("running git")?;
        if !out.status.success() {
            bail!(
                "git ls-remote {r}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        shas.extend(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().next().map(String::from)),
        );
    }
    shas.sort();
    shas.dedup();
    existing_commits(repo, &shas).await
}

/// The subset of `shas` that are commits in the local object store.
pub async fn existing_commits(repo: &Path, shas: &[String]) -> Result<Vec<String>> {
    use tokio::io::AsyncWriteExt;
    if shas.is_empty() {
        return Ok(vec![]);
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("running git cat-file")?;
    let mut stdin = child.stdin.take().context("git stdin")?;
    let input = shas.join("\n") + "\n";
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let out = child.wait_with_output().await?;
    let _ = writer.await;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_suffix(" commit").map(String::from))
        .collect())
}

/// Where a commit may already be kept, for [`commits_only_here`].
#[derive(Debug, Clone)]
pub enum RemoteAnchors {
    /// Tips that exist on the remotes now ([`live_remote_tips`]).
    Live(Vec<String>),
    /// Remotes unreachable: fall back to local remote-tracking refs (possibly stale).
    TrackingRefs,
}

/// Commits reachable from `rev` and from nothing else that survives a close: no remote
/// branch (per `anchors`), no tag, no local branch other than `own_branch`, nor any of
/// `also` (e.g. the head of a merged review). These are exactly what deleting `own_branch`
/// would lose. `--oneline`, newest first, at most `limit`.
pub async fn commits_only_here(
    repo: &Path,
    rev: &str,
    own_branch: Option<&str>,
    anchors: &RemoteAnchors,
    also: &[String],
    limit: usize,
) -> Result<Vec<String>> {
    let n = format!("-n{limit}");
    // `--exclude` patterns for `--branches` are relative to refs/heads (git-rev-list(1)).
    let excl = own_branch.map(|b| format!("--exclude={b}"));
    let mut args: Vec<&str> = vec!["rev-list", "--oneline", &n, rev, "--not"];
    if let Some(e) = &excl {
        args.push(e);
    }
    args.extend(["--branches", "--tags"]);
    match anchors {
        RemoteAnchors::Live(shas) => args.extend(shas.iter().map(String::as_str)),
        RemoteAnchors::TrackingRefs => args.push("--remotes"),
    }
    args.extend(also.iter().map(String::as_str));
    Ok(git(repo, &args)
        .await?
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// `git merge-base --is-ancestor <a> <b>`: `a` is `b` or one of its ancestors. False when
/// either is not a commit in this clone.
pub async fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", a, b])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Where the base branch (`main`) is, as reachability anchors: the local branch, every
/// remote-tracking `<remote>/<base>`, and the tip `git ls-remote` reports for each remote
/// (when that commit is present locally). The second value is a note when a remote could not
/// be reached (the local refs are still used).
pub async fn base_anchors(repo: &Path, base: &str) -> (Vec<String>, Option<String>) {
    let mut anchors = vec![];
    if rev_parse(repo, &format!("refs/heads/{base}"))
        .await
        .is_some()
    {
        anchors.push(format!("refs/heads/{base}"));
    }
    let remotes: Vec<String> = git(repo, &["remote"])
        .await
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    let mut live = vec![];
    let mut note = None;
    for r in &remotes {
        let tracking = format!("refs/remotes/{r}/{base}");
        if rev_parse(repo, &tracking).await.is_some() {
            anchors.push(tracking);
        }
        let fut = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["ls-remote", "--heads", r, &format!("refs/heads/{base}")])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(std::time::Duration::from_secs(20), fut).await {
            Ok(Ok(o)) if o.status.success() => live.extend(
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.split_whitespace().next().map(String::from)),
            ),
            _ => {
                note = Some(format!(
                    "remote {r} unreachable: \"merged into {base}\" uses {r}/{base} from the last fetch"
                ))
            }
        }
    }
    anchors.extend(existing_commits(repo, &live).await.unwrap_or_default());
    (anchors, note)
}

/// Commits of `rev` that are in none of `anchors` (refs or shas): `--oneline`, newest first,
/// at most `limit`. With the base branch as anchors these are the commits not merged into it.
pub async fn commits_not_in(
    repo: &Path,
    rev: &str,
    anchors: &[String],
    limit: usize,
) -> Result<Vec<String>> {
    let n = format!("-n{limit}");
    let mut args: Vec<&str> = vec!["rev-list", "--oneline", &n, rev, "--not"];
    args.extend(anchors.iter().map(String::as_str));
    Ok(git(repo, &args)
        .await?
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Local branches checked out in no worktree, whose name is `name` or ends in `/name`.
pub async fn loose_branches_named(repo: &Path, name: &str) -> Result<Vec<String>> {
    let checked_out: Vec<String> = porcelain(repo)
        .await?
        .into_iter()
        .filter_map(|e| e.branch)
        .collect();
    let suffix = format!("/{name}");
    Ok(git(
        repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .await?
    .lines()
    .filter(|b| *b == name || b.ends_with(&suffix))
    .filter(|b| !checked_out.iter().any(|c| c == b))
    .map(String::from)
    .collect())
}

pub async fn rev_parse(repo: &Path, rev: &str) -> Option<String> {
    git_ok(repo, &["rev-parse", "--verify", "--quiet", rev]).await
}

/// `-D`: deletes even an unmerged branch (callers check what would be lost first).
pub async fn branch_delete_force(repo: &Path, branch: &str) -> Result<()> {
    git(repo, &["branch", "-D", branch]).await.map(|_| ())
}

/// Re-create `branch` at `sha` (no-op when it already points there), optionally restoring
/// its upstream (`origin/feat/x`), which `branch -D` drops with the branch's config.
pub async fn branch_restore(
    repo: &Path,
    branch: &str,
    sha: &str,
    upstream: Option<&str>,
) -> Result<()> {
    let r = format!("refs/heads/{branch}");
    match rev_parse(repo, &r).await {
        Some(cur) if cur == sha => {}
        Some(cur) => bail!("branch {branch} exists at {cur}, expected {sha}"),
        None => {
            git(repo, &["branch", branch, sha]).await?;
        }
    }
    if let Some(u) = upstream {
        // Best effort: the remote branch may be gone by now.
        let _ = git(repo, &["branch", &format!("--set-upstream-to={u}"), branch]).await;
    }
    Ok(())
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
        assert!(v[0].locked.is_none());
        let l = parse_porcelain(
            "worktree /a/x\nHEAD 1\nbranch refs/heads/x\nlocked\n\nworktree /a/y\nHEAD 2\ndetached\nlocked on usb\n",
        );
        assert_eq!(l[0].locked.as_deref(), Some(""));
        assert_eq!(l[1].locked.as_deref(), Some("on usb"));
    }
}
