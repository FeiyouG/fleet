//! `fleet worktree gc`: list everything that can be cleaned, then clean it in one go.
//! Candidates are finished worktrees (`gc_verdict` = yes: merged, folder missing,
//! stale tmp, review gone), each closed through the same planner and transaction engine as
//! `fleet worktree close` (never forced: a blocked one is listed as blocked); orphans (dead
//! tmux sessions, tasks whose worktree is gone, stale rename links) get one small
//! transaction each. Transactions are independent: one failing is rolled back on its own.
//! Apply re-plans from a fresh snapshot and skips an item that changed since the dry run.

use super::close::{self, CloseOpts, Resolved};
use super::txn::{self, Step, Txn, TxnReport};
use super::Progress;
use crate::config::Config;
use crate::model::{Fleet, GcVerdict};
use crate::procs::canon;
use crate::registry::Registry;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Default)]
pub struct GcPlan {
    pub txns: Vec<Txn>,
    /// Finished worktrees that close would refuse (dirty, not merged, busy agent, locked), and why.
    pub blocked: Vec<(String, String)>,
}

pub async fn plan(fleet: &Fleet, cfg: &Config) -> GcPlan {
    let mut p = GcPlan::default();
    let opts = CloseOpts { force: false };

    for w in &fleet.worktrees {
        match &w.gc {
            GcVerdict::Yes(reason) => {
                let Some(project) = cfg.project(&w.worktree.repo).cloned() else {
                    continue;
                };
                let r = Resolved::worktree(project, w.worktree.path.clone());
                let planned = match close::gather(&r, fleet).await {
                    Ok(f) => close::plan(&f, &opts),
                    Err(e) => Err(e),
                };
                match planned {
                    Err(e) => p.blocked.push((w.worktree.name.clone(), format!("{e:#}"))),
                    Ok(cp) if cp.blocked() => p.blocked.push((
                        w.worktree.name.clone(),
                        cp.blockers
                            .iter()
                            .map(close::Blocker::summary)
                            .collect::<Vec<_>>()
                            .join("; "),
                    )),
                    Ok(mut cp) => {
                        cp.txn.label = format!("{} ({reason})", cp.txn.label);
                        p.txns.push(cp.txn);
                    }
                }
            }
            GcVerdict::No(reason) if reason == "dirty" => {
                p.blocked.push((w.worktree.name.clone(), reason.clone()));
            }
            _ => {}
        }
    }

    let covered =
        |p: &GcPlan, f: &dyn Fn(&Step) -> bool| p.txns.iter().any(|t| t.steps.iter().any(f));

    // tmux sessions whose cwd is gone.
    let mut seen = BTreeSet::new();
    for v in &fleet.panes {
        let s = &v.pane.session;
        if !v.pane.cwd.exists()
            && seen.insert(s.clone())
            && !covered(
                &p,
                &|x| matches!(x, Step::KillSession { session, .. } if session == s),
            )
        {
            p.txns.push(Txn::new(
                format!("kill tmux session {s} (cwd gone)"),
                vec![Step::KillSession {
                    session: s.clone(),
                    hosts_self: false,
                }],
            ));
        }
    }

    // Registry: tasks whose worktree is gone; rename links whose agent is gone.
    if let Ok(tasks) = Registry::open().and_then(|r| r.open_tasks()) {
        for t in tasks {
            if covered(
                &p,
                &|x| matches!(x, Step::CloseTask { slug } if *slug == t.slug),
            ) {
                continue;
            }
            // Registry paths are as written (`/tmp/...`); worktree rows are canonical.
            let t_path = canon(&t.path);
            let wt = fleet
                .worktrees
                .iter()
                .find(|w| canon(&w.worktree.path) == t_path);
            let alive = wt.map(|w| !w.agents.is_empty()).unwrap_or(false);
            let links: Vec<Step> = t
                .old_paths
                .iter()
                .filter(|o| o.is_symlink())
                .filter_map(|o| {
                    std::fs::read_link(o)
                        .ok()
                        .map(|target| Step::RemoveSymlink {
                            path: o.clone(),
                            target,
                        })
                })
                .collect();
            // A task is closed only when its worktree is gone. A stopped agent or a killed
            // tmux session alone keeps the task open.
            let wt_gone = wt.map(|w| w.worktree.missing).unwrap_or(true);
            if wt_gone {
                let mut steps = vec![Step::CloseTask {
                    slug: t.slug.clone(),
                }];
                steps.extend(links);
                p.txns.push(Txn::new(
                    format!("close registry task {} (worktree gone)", t.slug),
                    steps,
                ));
            } else if !alive && !links.is_empty() {
                p.txns.push(Txn::new(
                    format!("remove rename links of {} (agent gone)", t.slug),
                    links,
                ));
            }
        }
    }
    p
}

/// Apply the chosen items. `fresh` is a new snapshot: every item is planned again and run
/// only when its steps are unchanged, so an agent started (or a file changed) while the dry
/// run was on screen skips that item instead of being killed with it.
pub async fn apply(plan: &GcPlan, fresh: &Fleet, cfg: &Config, log: &Progress) -> Vec<TxnReport> {
    let now = self::plan(fresh, cfg).await;
    let mut out = vec![];
    for t in &plan.txns {
        match now.txns.iter().find(|n| *n == t) {
            Some(n) => out.push(txn::apply(n, &txn::Real, log).await),
            None => {
                let why = match now
                    .blocked
                    .iter()
                    .find(|(name, _)| t.label.contains(name.as_str()))
                {
                    Some((_, r)) => format!("now blocked: {r}"),
                    None => "changed since the dry run; run gc again".into(),
                };
                log(&format!("{}: skipped ({why})", t.label));
                out.push(TxnReport {
                    label: t.label.clone(),
                    skipped: Some(why),
                    ..Default::default()
                });
            }
        }
    }
    out
}

/// One line per transaction: its label and its steps.
pub fn describe(t: &Txn) -> String {
    format!(
        "{}: {}",
        t.label,
        t.steps
            .iter()
            .map(Step::describe)
            .collect::<Vec<_>>()
            .join("; ")
    )
}
