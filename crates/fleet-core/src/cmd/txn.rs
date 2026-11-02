//! All-or-nothing cleanup: an ordered list of [`Step`]s that either all happen, or, when one
//! fails, the ones already done are undone in reverse order. Used by `fleet worktree close`
//! and `fleet worktree gc`.
//!
//! The order lives in one place, [`Step::rank`]: cheap reversible steps first (so most
//! failures happen before anything is lost), agents stopped before their folder goes, the
//! worktree before its branch (git refuses to delete a checked-out branch), tmux kills last
//! (they are the only irreversible step of a normal close), and the session hosting this very
//! `fleet` process after everything else.
//!
//! Every step carries what its own undo needs, captured at plan time, and running a step whose
//! target is already gone succeeds, so re-running an interrupted close finishes the job.

use super::Progress;
use crate::registry::Registry;
use crate::tmux;
use crate::worktrees::git;
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How to re-create a removed worktree.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "rev")]
pub enum Restore {
    /// `git worktree add <path> <branch>`
    Branch(String),
    /// `git worktree add --detach <path> <sha>`
    Detached(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Step {
    /// Registry `closed_at`. Undo: reopen.
    CloseTask { slug: String },
    /// A `fleet agent rename` link at an old path. Undo: re-create it.
    RemoveSymlink { path: PathBuf, target: PathBuf },
    /// Kill an agent (its tmux pane, else SIGTERM) and wait for it to exit. No undo; the
    /// conversation survives (`opencode -s <session_id>`).
    StopAgent {
        pid: u32,
        pane: Option<String>,
        session_id: Option<String>,
        hosts_self: bool,
    },
    /// `git worktree remove` (`force`: 0 refuse dirty, 1 `-f`, 2 `-f -f` for a locked one).
    /// Undo: `git worktree add` from `restore` (uncommitted files are not restorable).
    RemoveWorktree {
        repo: PathBuf,
        path: PathBuf,
        restore: Restore,
        force: u8,
        /// The folder was already gone: only git's entry is dropped, nothing to re-create.
        missing: bool,
    },
    /// Delete a local branch. Undo: `git branch <branch> <sha>` (+ upstream).
    /// `best_effort` (gc): `git branch -d`, and a refusal (unmerged) is not a failure.
    DeleteBranch {
        repo: PathBuf,
        branch: String,
        sha: String,
        upstream: Option<String>,
        best_effort: bool,
    },
    // A future `--remote` adds `DeleteRemoteBranch { remote, branch, sha }` ranked right after
    // `DeleteBranch` (undo: push the sha back).
    /// Kill one pane of a session that also has panes outside the worktree. No undo.
    KillPane { target: String, hosts_self: bool },
    /// Kill a whole session. No undo.
    KillSession { session: String, hosts_self: bool },
}

impl Step {
    /// Position in a transaction; see the module docs for why.
    pub fn rank(&self) -> u8 {
        match self {
            Step::CloseTask { .. } => 1,
            Step::RemoveSymlink { .. } => 2,
            Step::StopAgent { hosts_self, .. } => {
                if *hosts_self {
                    7
                } else {
                    3
                }
            }
            Step::RemoveWorktree { .. } => 4,
            Step::DeleteBranch { .. } => 5,
            Step::KillPane { hosts_self, .. } | Step::KillSession { hosts_self, .. } => {
                if *hosts_self {
                    7
                } else {
                    6
                }
            }
        }
    }

    /// Whether this step's target is the tmux pane/session/agent running this `fleet`.
    pub fn hosts_self(&self) -> bool {
        matches!(
            self,
            Step::StopAgent {
                hosts_self: true,
                ..
            } | Step::KillPane {
                hosts_self: true,
                ..
            } | Step::KillSession {
                hosts_self: true,
                ..
            }
        )
    }

    pub fn reversible(&self) -> bool {
        matches!(
            self,
            Step::CloseTask { .. }
                | Step::RemoveSymlink { .. }
                | Step::RemoveWorktree { .. }
                | Step::DeleteBranch { .. }
        )
    }

    pub fn describe(&self) -> String {
        match self {
            Step::CloseTask { slug } => format!("close registry task {slug}"),
            Step::RemoveSymlink { path, target } => {
                format!("remove symlink {} → {}", tilde(path), tilde(target))
            }
            Step::StopAgent {
                pid,
                pane,
                hosts_self,
                ..
            } => format!(
                "stop agent pid {pid}{}{}",
                pane.as_deref()
                    .map(|p| format!(" (pane {p})"))
                    .unwrap_or_default(),
                if *hosts_self {
                    " — runs this fleet"
                } else {
                    ""
                }
            ),
            Step::RemoveWorktree {
                path,
                force,
                missing,
                ..
            } => format!(
                "remove worktree {}{}",
                tilde(path),
                match (missing, force) {
                    (true, _) => " (folder already gone)",
                    (_, 0) => "",
                    (_, 1) => " (--force)",
                    _ => " (--force --force: locked)",
                }
            ),
            Step::DeleteBranch {
                branch,
                sha,
                best_effort,
                ..
            } => format!(
                "delete branch {branch} (at {}){}",
                short(sha),
                if *best_effort { ", if merged" } else { "" }
            ),
            Step::KillPane { target, hosts_self } => format!(
                "kill tmux pane {target}{}",
                if *hosts_self {
                    " — runs this fleet"
                } else {
                    ""
                }
            ),
            Step::KillSession {
                session,
                hosts_self,
            } => format!(
                "kill tmux session {session}{}",
                if *hosts_self {
                    " — runs this fleet"
                } else {
                    ""
                }
            ),
        }
    }

    /// What a rollback does for this step, or why it cannot.
    pub fn undo_label(&self) -> String {
        match self {
            Step::CloseTask { .. } => "undo: reopen".into(),
            Step::RemoveSymlink { .. } => "undo: re-create".into(),
            Step::StopAgent { session_id, .. } => format!(
                "cannot be undone{}",
                session_id
                    .as_deref()
                    .map(|s| format!(" (resume: opencode -s {s})"))
                    .unwrap_or_default()
            ),
            Step::RemoveWorktree {
                restore,
                force,
                missing,
                ..
            } => {
                if *missing {
                    "undo: nothing to re-create".into()
                } else {
                    let r = match restore {
                        Restore::Branch(b) => format!("undo: git worktree add {b}"),
                        Restore::Detached(s) => {
                            format!("undo: git worktree add --detach {}", short(s))
                        }
                    };
                    if *force > 0 {
                        format!("{r} (uncommitted files lost)")
                    } else {
                        r
                    }
                }
            }
            Step::DeleteBranch { branch, sha, .. } => {
                format!("undo: git branch {branch} {}", short(sha))
            }
            Step::KillPane { .. } | Step::KillSession { .. } => "cannot be undone".into(),
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

pub fn tilde(p: &Path) -> String {
    let s = p.display().to_string();
    match dirs::home_dir() {
        Some(h) => {
            let h = h.display().to_string();
            match s.strip_prefix(&h) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                _ => s,
            }
        }
        None => s,
    }
}

/// An ordered list of steps that is applied all-or-nothing.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Txn {
    pub label: String,
    pub steps: Vec<Step>,
}

impl Txn {
    /// Sorts the steps into their safe order (stable: equal ranks keep their input order).
    pub fn new(label: impl Into<String>, mut steps: Vec<Step>) -> Self {
        steps.sort_by_key(Step::rank);
        Self {
            label: label.into(),
            steps,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RolledBack {
    pub step: Step,
    /// `None` = restored.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TxnReport {
    pub label: String,
    pub done: Vec<Step>,
    pub failed: Option<(Step, String)>,
    pub rolled_back: Vec<RolledBack>,
    /// Steps that ran before the failure and cannot be undone (agents stopped, tmux killed).
    pub not_restorable: Vec<Step>,
}

impl TxnReport {
    pub fn ok(&self) -> bool {
        self.failed.is_none()
    }
    pub fn rollback_complete(&self) -> bool {
        self.rolled_back.iter().all(|r| r.error.is_none())
    }
    /// Human summary of a failure (empty when ok).
    pub fn failure_lines(&self) -> Vec<String> {
        let Some((s, e)) = &self.failed else {
            return vec![];
        };
        let mut v = vec![format!("FAILED: {}: {e}", s.describe())];
        for r in &self.rolled_back {
            v.push(match &r.error {
                None => format!("  rolled back: {}", r.step.describe()),
                Some(e) => format!("  ROLLBACK FAILED: {}: {e}", r.step.describe()),
            });
        }
        for s in &self.not_restorable {
            v.push(format!(
                "  not restorable: {} ({})",
                s.describe(),
                s.undo_label()
            ));
        }
        v.push(if !self.rollback_complete() {
            "rollback incomplete: the items marked ROLLBACK FAILED are left as they are now".into()
        } else if self.not_restorable.is_empty() {
            "everything was restored; nothing lost".into()
        } else {
            "everything else was restored".into()
        });
        v
    }
}

/// Runs and undoes steps. [`Real`] touches the world; tests use a fake.
#[async_trait]
pub trait Exec: Send + Sync {
    async fn run(&self, s: &Step) -> Result<()>;
    async fn undo(&self, s: &Step) -> Result<()>;
}

pub async fn apply(t: &Txn, exec: &dyn Exec, log: &Progress) -> TxnReport {
    let mut rep = TxnReport {
        label: t.label.clone(),
        ..Default::default()
    };
    for s in &t.steps {
        if s.hosts_self() {
            log(&format!(
                "{}: done except the tmux pane/agent running this fleet, killed now",
                t.label
            ));
        }
        match exec.run(s).await {
            Ok(()) => {
                log(&format!("{}: ok", s.describe()));
                rep.done.push(s.clone());
            }
            Err(e) => {
                log(&format!("{}: FAILED: {e:#}", s.describe()));
                rep.failed = Some((s.clone(), format!("{e:#}")));
                break;
            }
        }
    }
    if rep.failed.is_some() {
        for s in rep.done.iter().rev() {
            if s.reversible() {
                let r = exec.undo(s).await;
                log(&format!(
                    "rollback {}: {}",
                    s.describe(),
                    match &r {
                        Ok(()) => "ok".into(),
                        Err(e) => format!("FAILED: {e:#}"),
                    }
                ));
                rep.rolled_back.push(RolledBack {
                    step: s.clone(),
                    error: r.err().map(|e| format!("{e:#}")),
                });
            } else {
                rep.not_restorable.push(s.clone());
            }
        }
    }
    rep
}

/// The real executor: registry, filesystem, git, tmux, signals.
pub struct Real;

#[async_trait]
impl Exec for Real {
    async fn run(&self, s: &Step) -> Result<()> {
        match s {
            Step::CloseTask { slug } => Registry::open()?.close(slug),
            Step::RemoveSymlink { path, .. } => match std::fs::remove_file(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r.map_err(Into::into),
            },
            Step::StopAgent { pid, pane, .. } => stop_pid(*pid, pane.as_deref()).await,
            Step::RemoveWorktree {
                repo,
                path,
                force,
                missing,
                ..
            } => {
                // Never sit in a folder that is being removed.
                if let Ok(cwd) = std::env::current_dir() {
                    if cwd.starts_with(path) || crate::procs::canon(&cwd).starts_with(path) {
                        std::env::set_current_dir(repo).ok();
                    }
                }
                let still_listed = git::porcelain(repo)
                    .await?
                    .iter()
                    .any(|e| e.path == *path || crate::procs::canon(&e.path) == *path);
                if !still_listed {
                    return Ok(());
                }
                git::worktree_remove(repo, path, if *missing { 0 } else { *force }).await
            }
            Step::DeleteBranch {
                repo,
                branch,
                best_effort,
                ..
            } => {
                if git::rev_parse(repo, &format!("refs/heads/{branch}"))
                    .await
                    .is_none()
                {
                    return Ok(());
                }
                if *best_effort {
                    let _ = git::branch_delete(repo, branch).await;
                    Ok(())
                } else {
                    git::branch_delete_force(repo, branch).await
                }
            }
            Step::KillPane { target, .. } => tmux::kill_pane(target).await,
            Step::KillSession { session, .. } => tmux::kill_session(session).await,
        }
    }

    async fn undo(&self, s: &Step) -> Result<()> {
        match s {
            Step::CloseTask { slug } => Registry::open()?.reopen(slug),
            Step::RemoveSymlink { path, target } => {
                if path.is_symlink() {
                    return Ok(());
                }
                std::os::unix::fs::symlink(target, path).map_err(Into::into)
            }
            Step::RemoveWorktree {
                repo,
                path,
                restore,
                missing,
                ..
            } => {
                if *missing || path.exists() {
                    return Ok(());
                }
                match restore {
                    Restore::Branch(b) => git::worktree_add_existing(repo, path, b).await,
                    Restore::Detached(sha) => git::worktree_add_detached(repo, path, sha).await,
                }
            }
            Step::DeleteBranch {
                repo,
                branch,
                sha,
                upstream,
                ..
            } => git::branch_restore(repo, branch, sha, upstream.as_deref()).await,
            other => bail!("{} cannot be undone", other.describe()),
        }
    }
}

async fn pid_alive(pid: u32) -> bool {
    tokio::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn wait_exit(pid: u32, within: Duration) -> bool {
    let until = std::time::Instant::now() + within;
    while std::time::Instant::now() < until {
        if !pid_alive(pid).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !pid_alive(pid).await
}

/// Kill the agent's pane (SIGHUP to everything in it), else SIGTERM; SIGKILL after 5 s.
/// Returns once the process is gone. Shared with `fleet agent stop`.
pub async fn stop_pid(pid: u32, pane: Option<&str>) -> Result<()> {
    if !pid_alive(pid).await {
        return Ok(());
    }
    match pane {
        Some(p) => tmux::kill_pane(p).await?,
        None => {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
        }
    }
    if wait_exit(pid, Duration::from_secs(5)).await {
        return Ok(());
    }
    let _ = tokio::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .await;
    if wait_exit(pid, Duration::from_secs(2)).await {
        Ok(())
    } else {
        bail!("agent pid {pid} is still running")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records calls; fails `run` at index `fail_at` and `undo` of steps in `undo_fails`.
    struct Fake {
        fail_at: Option<usize>,
        undo_fails: Vec<u8>,
        calls: Mutex<Vec<String>>,
        n: Mutex<usize>,
    }

    impl Fake {
        fn new(fail_at: Option<usize>) -> Self {
            Self {
                fail_at,
                undo_fails: vec![],
                calls: Mutex::new(vec![]),
                n: Mutex::new(0),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Exec for Fake {
        async fn run(&self, s: &Step) -> Result<()> {
            let mut n = self.n.lock().unwrap();
            let i = *n;
            *n += 1;
            self.calls.lock().unwrap().push(format!("run {}", s.rank()));
            if Some(i) == self.fail_at {
                bail!("boom")
            }
            Ok(())
        }
        async fn undo(&self, s: &Step) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("undo {}", s.rank()));
            if self.undo_fails.contains(&s.rank()) {
                bail!("undo boom")
            }
            Ok(())
        }
    }

    fn all_steps() -> Vec<Step> {
        // Deliberately out of order.
        vec![
            Step::KillSession {
                session: "t".into(),
                hosts_self: false,
            },
            Step::DeleteBranch {
                repo: "/r".into(),
                branch: "feat/t".into(),
                sha: "abc".into(),
                upstream: None,
                best_effort: false,
            },
            Step::KillPane {
                target: "me:1.0".into(),
                hosts_self: true,
            },
            Step::RemoveWorktree {
                repo: "/r".into(),
                path: "/w/t".into(),
                restore: Restore::Branch("feat/t".into()),
                force: 0,
                missing: false,
            },
            Step::StopAgent {
                pid: 7,
                pane: None,
                session_id: None,
                hosts_self: false,
            },
            Step::RemoveSymlink {
                path: "/w/old".into(),
                target: "/w/t".into(),
            },
            Step::CloseTask { slug: "t".into() },
        ]
    }

    #[test]
    fn new_sorts_into_safe_order() {
        let t = Txn::new("x", all_steps());
        let ranks: Vec<u8> = t.steps.iter().map(Step::rank).collect();
        assert_eq!(ranks, vec![1, 2, 3, 4, 5, 6, 7]);
        assert!(t.steps.last().unwrap().hosts_self());
    }

    #[tokio::test]
    async fn success_runs_everything_in_order() {
        let t = Txn::new("x", all_steps());
        let f = Fake::new(None);
        let r = apply(&t, &f, &super::super::quiet()).await;
        assert!(r.ok());
        assert_eq!(r.done.len(), 7);
        assert!(f.calls().iter().all(|c| c.starts_with("run")));
    }

    #[tokio::test]
    async fn failure_at_every_index_undoes_reversible_done_steps_in_reverse() {
        let t = Txn::new("x", all_steps());
        for k in 0..t.steps.len() {
            let f = Fake::new(Some(k));
            let r = apply(&t, &f, &super::super::quiet()).await;
            assert!(!r.ok());
            assert_eq!(r.failed.as_ref().unwrap().0, t.steps[k]);
            let done = &t.steps[..k];
            let want_undo: Vec<String> = done
                .iter()
                .rev()
                .filter(|s| s.reversible())
                .map(|s| format!("undo {}", s.rank()))
                .collect();
            let got_undo: Vec<String> = f
                .calls()
                .into_iter()
                .filter(|c| c.starts_with("undo"))
                .collect();
            assert_eq!(got_undo, want_undo, "fail at {k}");
            let want_lost: Vec<&Step> = done.iter().rev().filter(|s| !s.reversible()).collect();
            assert_eq!(r.not_restorable.iter().collect::<Vec<_>>(), want_lost);
            // Nothing after the failed step ran.
            assert_eq!(
                f.calls().iter().filter(|c| c.starts_with("run")).count(),
                k + 1
            );
            assert!(r.rollback_complete());
        }
    }

    #[tokio::test]
    async fn failing_undo_is_reported_and_the_rest_still_rolls_back() {
        let t = Txn::new("x", all_steps());
        let mut f = Fake::new(Some(5)); // first kill (rank 6)
        f.undo_fails = vec![4]; // re-adding the worktree fails
        let r = apply(&t, &f, &super::super::quiet()).await;
        assert!(!r.rollback_complete());
        let undone: Vec<u8> = r.rolled_back.iter().map(|x| x.step.rank()).collect();
        assert_eq!(undone, vec![5, 4, 2, 1]);
        assert!(r.rolled_back[1].error.is_some());
        assert!(r
            .failure_lines()
            .iter()
            .any(|l| l.starts_with("rollback incomplete")));
    }
}
