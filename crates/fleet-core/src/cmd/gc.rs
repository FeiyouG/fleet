//! `fleet worktree gc`: dry run first, then apply.

use super::Progress;
use crate::config::Config;
use crate::model::{Fleet, GcVerdict};
use crate::registry::Registry;
use crate::tmux;
use crate::worktrees::git;
use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GcAction {
    RemoveWorktree {
        repo: String,
        path: PathBuf,
        branch: Option<String>,
        reason: String,
        missing: bool,
    },
    KillTmux {
        session: String,
        reason: String,
    },
    RemoveSymlink {
        path: PathBuf,
        reason: String,
    },
    CloseTask {
        slug: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GcPlan {
    pub actions: Vec<GcAction>,
    /// Worktrees that would qualify but are protected (dirty / unpushed / agent alive).
    pub blocked: Vec<(String, String)>,
}

pub fn plan(fleet: &Fleet, cfg: &Config) -> GcPlan {
    let mut p = GcPlan::default();
    let live_sessions: std::collections::HashSet<&str> = fleet
        .panes
        .iter()
        .map(|v| v.pane.session.as_str())
        .collect();

    for w in &fleet.worktrees {
        match &w.gc {
            GcVerdict::Yes(reason) => {
                p.actions.push(GcAction::RemoveWorktree {
                    repo: w.worktree.repo.clone(),
                    path: w.worktree.path.clone(),
                    branch: w.worktree.branch.clone(),
                    reason: reason.clone(),
                    missing: w.worktree.missing,
                });
                for s in w
                    .panes
                    .iter()
                    .map(|x| x.session.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    p.actions.push(GcAction::KillTmux {
                        session: s,
                        reason: "worktree removed".into(),
                    });
                }
            }
            GcVerdict::No(reason) if matches!(reason.as_str(), "dirty" | "unpushed") => {
                p.blocked.push((w.worktree.name.clone(), reason.clone()));
            }
            _ => {}
        }
    }

    // tmux sessions whose cwd is gone.
    let mut seen = std::collections::BTreeSet::new();
    for v in &fleet.panes {
        if !v.pane.cwd.exists() && seen.insert(v.pane.session.clone()) {
            p.actions.push(GcAction::KillTmux {
                session: v.pane.session.clone(),
                reason: "cwd gone".into(),
            });
        }
    }

    // Registry: rename symlinks whose target has no agent; tasks whose worktree is gone.
    if let Ok(reg) = Registry::open() {
        if let Ok(tasks) = reg.open_tasks() {
            for t in tasks {
                let alive = fleet
                    .worktrees
                    .iter()
                    .any(|w| w.worktree.path == t.path && !w.agents.is_empty());
                for old in &t.old_paths {
                    if old.is_symlink() && !alive {
                        p.actions.push(GcAction::RemoveSymlink {
                            path: old.clone(),
                            reason: "rename link, agent gone".into(),
                        });
                    }
                }
                let wt_exists = fleet
                    .worktrees
                    .iter()
                    .any(|w| w.worktree.path == t.path && !w.worktree.missing);
                let removing = p
                    .actions
                    .iter()
                    .any(|a| matches!(a, GcAction::RemoveWorktree { path, .. } if *path == t.path));
                if !wt_exists || removing {
                    if !live_sessions.contains(t.tmux_session.as_str()) || removing {
                        p.actions.push(GcAction::CloseTask {
                            slug: t.slug.clone(),
                            reason: "worktree gone".into(),
                        });
                    }
                }
            }
        }
    }
    let _ = cfg;
    p
}

pub async fn apply(plan: &GcPlan, cfg: &Config, log: &Progress) -> Vec<(GcAction, Result<()>)> {
    let mut out = vec![];
    for a in &plan.actions {
        let r = match a {
            GcAction::RemoveWorktree {
                repo,
                path,
                branch,
                missing,
                ..
            } => {
                let repo_path = cfg.repo_by_name(repo).map(|r| r.path.clone());
                match repo_path {
                    None => Err(anyhow::anyhow!("repo {repo} not in config")),
                    Some(rp) => {
                        let r = if *missing {
                            git::worktree_prune(&rp).await
                        } else {
                            git::worktree_remove(&rp, path, false).await
                        };
                        if r.is_ok() {
                            if let Some(b) = branch {
                                // Safe delete only; a non-merged branch stays.
                                let _ = git::branch_delete(&rp, b).await;
                            }
                        }
                        r
                    }
                }
            }
            GcAction::KillTmux { session, .. } => tmux::kill_session(session).await,
            GcAction::RemoveSymlink { path, .. } => std::fs::remove_file(path).map_err(Into::into),
            GcAction::CloseTask { slug, .. } => Registry::open().and_then(|r| r.close(slug)),
        };
        log(&format!(
            "{}: {}",
            describe(a),
            match &r {
                Ok(()) => "ok".into(),
                Err(e) => format!("FAILED: {e}"),
            }
        ));
        out.push((a.clone(), r));
    }
    out
}

pub fn describe(a: &GcAction) -> String {
    match a {
        GcAction::RemoveWorktree { path, reason, .. } => {
            format!("remove worktree {} ({reason})", path.display())
        }
        GcAction::KillTmux { session, reason } => format!("kill tmux session {session} ({reason})"),
        GcAction::RemoveSymlink { path, reason } => {
            format!("remove symlink {} ({reason})", path.display())
        }
        GcAction::CloseTask { slug, reason } => format!("close registry task {slug} ({reason})"),
    }
}
