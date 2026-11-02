//! `fleet worktree close TARGET` (alias `fleet close`): finish a task and remove everything
//! that belonged to it — agents, tmux panes/sessions, the worktree, its local branch, the
//! registry task and rename symlinks — all-or-nothing (see [`super::txn`]).
//!
//! resolve (name → worktree) → gather (all reads) → plan (pure) → apply (re-check, then run).

use super::txn::{self, Exec, Restore, Step, Txn, TxnReport};
use super::Progress;
use crate::config::{expand, Config, Project};
use crate::model::{Activity, AgentRow, Fleet, ReviewStatus};
use crate::procs::{canon, ProcTable};
use crate::registry::Registry;
use crate::worktrees::git;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default)]
pub struct CloseOpts {
    /// Close despite blockers (uncommitted files, commits only here, running agents, lock).
    pub force: bool,
    /// gc mode: delete the branch only if merged (`git branch -d`), never fail on it.
    pub gc: bool,
    // A future `remote: bool` adds a `DeleteRemoteBranch` step in `plan`.
}

/// The worktree a target resolved to.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub project: Project,
    pub path: PathBuf,
}

/// Something that would be lost; `--force` closes anyway.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Blocker {
    /// `git status --porcelain` lines.
    Dirty {
        files: Vec<String>,
    },
    /// Commits on no remote branch, tag or other local branch (`--oneline`).
    Unpushed {
        commits: Vec<String>,
        more: bool,
    },
    AgentRunning {
        pid: u32,
        activity: Activity,
        pane: Option<String>,
        session_id: Option<String>,
    },
    Locked {
        reason: String,
    },
}

impl Blocker {
    /// A header line plus up to `limit` item lines.
    pub fn lines(&self, limit: usize) -> Vec<String> {
        let list = |head: String, items: &[String], more: bool| {
            let mut v = vec![head];
            v.extend(items.iter().take(limit).map(|i| format!("    {i}")));
            if items.len() > limit || more {
                v.push(format!(
                    "    … {}more",
                    if items.len() > limit {
                        format!("{} ", items.len() - limit)
                    } else {
                        String::new()
                    }
                ));
            }
            v
        };
        match self {
            Blocker::Dirty { files } => list(
                format!("uncommitted changes ({} files):", files.len()),
                files,
                false,
            ),
            Blocker::Unpushed { commits, more } => list(
                format!(
                    "commits only on this branch ({}{}), on no remote/tag/other branch:",
                    commits.len(),
                    if *more { "+" } else { "" }
                ),
                commits,
                *more,
            ),
            Blocker::AgentRunning {
                pid,
                activity,
                pane,
                session_id,
            } => vec![format!(
                "agent running: opencode pid {pid} {}{}{}",
                activity.label(),
                pane.as_deref()
                    .map(|p| format!(" in {p}"))
                    .unwrap_or_default(),
                session_id
                    .as_deref()
                    .map(|s| format!(" (resume later: opencode -s {s})"))
                    .unwrap_or_default()
            )],
            Blocker::Locked { reason } => vec![format!(
                "worktree is locked (git worktree lock){}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                }
            )],
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub project: String,
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    /// Registry slugs (fleet-started tasks on this worktree).
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFacts {
    pub slug: String,
    pub session: String,
    /// (old path, link target) of each rename symlink still on disk.
    pub symlinks: Vec<(PathBuf, PathBuf)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    pub name: String,
    /// Every pane of the session is inside the worktree (or it is the task's own session).
    pub whole: bool,
    /// Targets of its panes that are inside the worktree.
    pub panes: Vec<String>,
}

/// Everything `plan` needs, read once. Pure data so the planner is unit-testable.
#[derive(Debug, Clone)]
pub struct Facts {
    pub project: Project,
    pub path: PathBuf,
    pub name: String,
    pub is_main: bool,
    /// git still lists the worktree (false: an earlier close stopped after removing it).
    pub listed: bool,
    pub missing: bool,
    pub branch: Option<String>,
    /// Full sha.
    pub head: String,
    pub upstream: Option<String>,
    pub locked: Option<String>,
    pub dirty: Vec<String>,
    pub only_here: Vec<String>,
    pub only_here_more: bool,
    /// Display id of a merged review for this worktree (`!514`).
    pub review_merged: Option<String>,
    /// Why the branch must stay (base branch, checked out elsewhere).
    pub keep_branch: Option<String>,
    pub tasks: Vec<TaskFacts>,
    pub agents: Vec<AgentRow>,
    pub sessions: Vec<SessionFacts>,
    /// Pane targets / agent pids that are ancestors of this `fleet` process.
    pub host_panes: Vec<String>,
    pub host_agents: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClosePlan {
    pub target: Target,
    pub txn: Txn,
    pub blockers: Vec<Blocker>,
    pub notes: Vec<String>,
    pub force: bool,
    #[serde(skip)]
    pub facts: Facts,
}

impl ClosePlan {
    /// Blockers that stop this plan (none once forced).
    pub fn blocked(&self) -> bool {
        !self.force && !self.blockers.is_empty()
    }

    /// The same close, with `--force`.
    pub fn forced(&self, gc: bool) -> Result<ClosePlan> {
        plan(&self.facts, &CloseOpts { force: true, gc })
    }

    /// Every step with its undo, for the CLI and the TUI popup.
    pub fn step_lines(&self) -> Vec<(String, String)> {
        self.txn
            .steps
            .iter()
            .map(|s| (s.describe(), s.undo_label()))
            .collect()
    }

    pub fn blocker_lines(&self, limit: usize) -> Vec<String> {
        self.blockers.iter().flat_map(|b| b.lines(limit)).collect()
    }

    pub fn header(&self) -> String {
        let t = &self.target;
        format!(
            "close {} ({}{}) · {} on {}",
            t.name,
            t.project,
            if t.tasks.is_empty() {
                ""
            } else {
                ", fleet task"
            },
            txn::tilde(&t.path),
            t.branch
                .clone()
                .unwrap_or_else(|| format!("detached {}", &t.head[..t.head.len().min(8)]))
        )
    }
}

// ───────────────────────── resolve ─────────────────────────

fn looks_like_path(t: &str) -> bool {
    t.contains('/') || t == "." || t == ".." || t.starts_with('~')
}

/// The configured project whose main clone is `main`, else an ad-hoc one (any repo works).
pub fn project_for_main(cfg: &Config, main: &Path) -> Project {
    let m = canon(main);
    cfg.projects
        .iter()
        .find(|p| canon(&p.repo) == m)
        .cloned()
        .unwrap_or_else(|| Project {
            name: m
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".into()),
            repo: m.clone(),
            worktrees: None,
            reviews: None,
            base: "main".into(),
        })
}

/// Find the worktree `target` names: a path (any folder inside a worktree, or a rename
/// symlink), a registry task slug, a worktree folder name, a branch, or a tmux session.
/// `scope` (a project name) only breaks ties between several matches.
pub async fn resolve(
    fleet: &Fleet,
    cfg: &Config,
    target: &str,
    scope: Option<&str>,
) -> Result<Resolved> {
    if looks_like_path(target) {
        let p = expand(Path::new(target));
        let p = if p.is_relative() {
            std::env::current_dir()?.join(p)
        } else {
            p
        };
        if p.exists() {
            let p = canon(&p);
            let top = git::toplevel(&p)
                .await
                .with_context(|| format!("{} is not inside a git worktree", p.display()))?;
            let top = canon(&top);
            let main = git::main_clone(&top)
                .await
                .with_context(|| format!("cannot find the repo of {}", top.display()))?;
            return Ok(Resolved {
                project: project_for_main(cfg, &main),
                path: top,
            });
        }
        // A folder already deleted: only git's list still knows it.
        let hit = fleet
            .worktrees
            .iter()
            .find(|w| w.worktree.path == p || canon(&w.worktree.path) == canon(&p));
        if let Some(w) = hit {
            return Ok(Resolved {
                project: cfg
                    .project(&w.worktree.repo)
                    .cloned()
                    .context("project not in config")?,
                path: w.worktree.path.clone(),
            });
        }
        // Or a registry task left by a close that stopped half-way.
        let task = Registry::open()
            .and_then(|r| r.tasks_for_path(&p))
            .ok()
            .and_then(|v| v.into_iter().next());
        return match task.and_then(|t| cfg.project(&t.repo).cloned()) {
            Some(project) => Ok(Resolved { project, path: p }),
            None => bail!("{} does not exist and is no known worktree", p.display()),
        };
    }

    let tasks = Registry::open()
        .and_then(|r| r.open_tasks())
        .unwrap_or_default();
    let (path, repo) = resolve_name(fleet, &tasks, target, scope)?;
    let project = match cfg.project(&repo) {
        Some(p) => p.clone(),
        None => {
            let main = git::main_clone(&path)
                .await
                .with_context(|| format!("repo `{repo}` is not in config"))?;
            project_for_main(cfg, &main)
        }
    };
    Ok(Resolved { project, path })
}

/// Name lookup (pure): `(worktree path, project name)` for a task slug, worktree folder name,
/// branch or tmux session; ambiguity is an error unless `scope` picks one.
pub fn resolve_name(
    fleet: &Fleet,
    tasks: &[crate::registry::Task],
    target: &str,
    scope: Option<&str>,
) -> Result<(PathBuf, String)> {
    // path -> project, deduplicated by path.
    let mut cands: BTreeMap<PathBuf, String> = BTreeMap::new();
    for t in tasks {
        if t.slug == target {
            cands.insert(canon(&t.path), t.repo.clone());
        }
    }
    for w in &fleet.worktrees {
        let w = &w.worktree;
        if w.name == target || w.branch.as_deref() == Some(target) {
            cands.insert(w.path.clone(), w.repo.clone());
        }
    }
    for p in &fleet.panes {
        if p.pane.session == target {
            if let Some(w) = &p.worktree {
                cands.insert(w.path.clone(), w.repo.clone());
            }
        }
    }
    if cands.len() > 1 {
        if let Some(s) = scope {
            cands.retain(|_, repo| repo == s);
        }
    }
    let mut it = cands.into_iter();
    Ok(match (it.next(), it.next()) {
        (None, _) => {
            bail!("no task, worktree, branch or tmux session named `{target}` (a path works too)")
        }
        (Some(one), None) => one,
        (Some(a), Some(b)) => {
            let all: Vec<String> = [a, b]
                .into_iter()
                .chain(it)
                .map(|(p, r)| format!("{r}: {}", txn::tilde(&p)))
                .collect();
            bail!(
                "`{target}` matches several worktrees ({}); pass a path or --project",
                all.join(", ")
            )
        }
    })
}

// ───────────────────────── gather ─────────────────────────

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || canon(a) == canon(b)
}

/// Read everything about the resolved worktree: git, registry, snapshot (agents, panes),
/// and which pane/agent hosts this process.
pub async fn gather(r: &Resolved, fleet: &Fleet) -> Result<Facts> {
    let repo = &r.project.repo;
    let entries = git::porcelain(repo).await?;
    let reg_tasks = Registry::open()
        .and_then(|reg| reg.tasks_for_path(&r.path))
        .unwrap_or_default();
    let idx = entries.iter().position(|e| same_path(&e.path, &r.path));
    let e = match idx {
        Some(i) => entries[i].clone(),
        // A close that stopped half-way (crash, killed): git already forgot the worktree,
        // but the registry task and maybe the branch remain. Finish the job.
        None if !r.path.exists() && !reg_tasks.is_empty() => {
            let mut branch = None;
            let mut head = String::new();
            for b in reg_tasks.iter().filter_map(|t| t.branch.as_ref()) {
                if let Some(sha) = git::rev_parse(repo, &format!("refs/heads/{b}")).await {
                    branch = Some(b.clone());
                    head = sha;
                    break;
                }
            }
            git::Porcelain {
                path: r.path.clone(),
                head,
                branch,
                detached: false,
                bare: false,
                prunable: true,
                locked: None,
            }
        }
        None => bail!(
            "{} is not a worktree of {}",
            r.path.display(),
            repo.display()
        ),
    };
    let listed = idx.is_some();
    let idx = idx.unwrap_or(usize::MAX);
    let e = &e;
    let missing = e.prunable || !e.path.exists();
    let path = if missing {
        e.path.clone()
    } else {
        canon(&e.path)
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let upstream = match (&e.branch, missing) {
        (Some(b), false) => git::git(
            repo,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                &format!("{b}@{{u}}"),
            ],
        )
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()),
        _ => None,
    };
    let dirty = if missing || idx == 0 {
        vec![]
    } else {
        git::status_lines(&path).await?
    };
    const LIMIT: usize = 20;
    let mut only_here = if e.head.is_empty() || idx == 0 {
        vec![]
    } else {
        git::commits_only_here(repo, &e.head, e.branch.as_deref(), LIMIT + 1).await?
    };
    let only_here_more = only_here.len() > LIMIT;
    only_here.truncate(LIMIT);

    let review_merged = fleet
        .worktrees
        .iter()
        .filter(|w| same_path(&w.worktree.path, &path))
        .filter_map(|w| w.review.as_ref())
        .chain(
            fleet
                .reviews
                .iter()
                .filter(|v| v.worktrees.iter().any(|w| same_path(&w.path, &path)))
                .map(|v| &v.review),
        )
        .find(|r| r.status == ReviewStatus::Merged)
        .map(|r| r.display_id.clone());

    let keep_branch = e.branch.as_ref().and_then(|b| {
        if *b == r.project.base {
            return Some(format!("{b} is the project's base branch"));
        }
        entries
            .iter()
            .enumerate()
            .find(|(i, x)| *i != idx && x.branch.as_deref() == Some(b.as_str()))
            .map(|(_, x)| format!("{b} is also checked out in {}", txn::tilde(&x.path)))
    });

    let tasks: Vec<TaskFacts> = reg_tasks
        .into_iter()
        .map(|t| TaskFacts {
            symlinks: t
                .old_paths
                .iter()
                .filter(|o| o.is_symlink())
                .filter_map(|o| std::fs::read_link(o).ok().map(|tg| (o.clone(), tg)))
                .collect(),
            slug: t.slug,
            session: t.tmux_session,
        })
        .collect();

    // Inside this worktree and not inside a nested, longer one.
    let others: Vec<PathBuf> = entries
        .iter()
        .enumerate()
        .filter(|(i, x)| *i != idx && !x.prunable)
        .map(|(_, x)| canon(&x.path))
        .filter(|x| x.starts_with(&path) && x.as_os_str().len() > path.as_os_str().len())
        .collect();
    let inside = |p: &Path| -> bool {
        !missing && p.starts_with(&path) && !others.iter().any(|o| p.starts_with(o))
    };

    let mut agents: Vec<AgentRow> = vec![];
    for row in &fleet.overview {
        for a in &row.agents {
            if inside(&a.cwd) && !agents.iter().any(|x| x.pid == a.pid) {
                agents.push(a.clone());
            }
        }
    }
    let mut by_session: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in &fleet.panes {
        if inside(&canon(&v.pane.cwd)) {
            by_session
                .entry(v.pane.session.clone())
                .or_default()
                .push(v.pane.target());
        }
    }
    let mut sessions: Vec<SessionFacts> = by_session
        .into_iter()
        .map(|(name, panes)| {
            let total = fleet
                .panes
                .iter()
                .filter(|v| v.pane.session == name)
                .count();
            let own = tasks.iter().any(|t| t.session == name);
            SessionFacts {
                whole: own || panes.len() == total,
                name,
                panes,
            }
        })
        .collect();
    for t in &tasks {
        let exists = fleet.panes.iter().any(|v| v.pane.session == t.session);
        if exists && !sessions.iter().any(|s| s.name == t.session) {
            sessions.push(SessionFacts {
                name: t.session.clone(),
                whole: true,
                panes: vec![],
            });
        }
    }

    let anc = tokio::task::spawn_blocking(|| ProcTable::snapshot().ancestors(std::process::id()))
        .await
        .unwrap_or_default();
    let host_panes = fleet
        .panes
        .iter()
        .filter(|v| anc.contains(&v.pane.pid))
        .map(|v| v.pane.target())
        .collect();
    let host_agents = agents
        .iter()
        .filter(|a| anc.contains(&a.pid))
        .map(|a| a.pid)
        .collect();

    Ok(Facts {
        project: r.project.clone(),
        path,
        name,
        is_main: idx == 0 && !e.bare,
        listed,
        missing,
        branch: e.branch.clone(),
        head: e.head.clone(),
        upstream,
        locked: e.locked.clone(),
        dirty,
        only_here,
        only_here_more,
        review_merged,
        keep_branch,
        tasks,
        agents,
        sessions,
        host_panes,
        host_agents,
    })
}

// ───────────────────────── plan (pure) ─────────────────────────

pub fn plan(f: &Facts, opts: &CloseOpts) -> Result<ClosePlan> {
    if f.is_main {
        bail!(
            "{} is the main clone of {}; close removes worktrees, never the repo itself",
            txn::tilde(&f.path),
            f.project.name
        );
    }
    let mut blockers = vec![];
    let mut notes = vec![];
    if !f.missing && !f.dirty.is_empty() {
        blockers.push(Blocker::Dirty {
            files: f.dirty.clone(),
        });
    }
    if !f.only_here.is_empty() {
        match &f.review_merged {
            Some(id) => notes.push(format!(
                "{id} is merged, so {} commit(s) found only on this branch are treated as landed (squash/rebase merge): {}",
                f.only_here.len(),
                f.only_here.join(" · ")
            )),
            None => blockers.push(Blocker::Unpushed {
                commits: f.only_here.clone(),
                more: f.only_here_more,
            }),
        }
    }
    for a in &f.agents {
        blockers.push(Blocker::AgentRunning {
            pid: a.pid,
            activity: a.activity,
            pane: a.pane_target.clone(),
            session_id: a.session_id.clone(),
        });
    }
    if let (Some(r), false) = (&f.locked, f.missing) {
        blockers.push(Blocker::Locked { reason: r.clone() });
    }
    if !f.listed {
        notes.push(
            "git no longer lists this worktree (an earlier close stopped half-way): finishing it"
                .into(),
        );
    } else if f.missing {
        notes.push("the folder is already gone; only git's entry is removed".into());
    }

    let mut steps = vec![];
    for t in &f.tasks {
        steps.push(Step::CloseTask {
            slug: t.slug.clone(),
        });
        for (path, target) in &t.symlinks {
            steps.push(Step::RemoveSymlink {
                path: path.clone(),
                target: target.clone(),
            });
        }
    }
    let host_session = |s: &str| f.host_panes.iter().any(|p| p.split(':').next() == Some(s));
    let mut agent_panes = vec![];
    for a in &f.agents {
        let pane_hosts = a
            .pane_target
            .as_ref()
            .map(|p| f.host_panes.contains(p))
            .unwrap_or(false);
        if let Some(p) = &a.pane_target {
            agent_panes.push(p.clone());
        }
        steps.push(Step::StopAgent {
            pid: a.pid,
            pane: a.pane_target.clone(),
            session_id: a.session_id.clone(),
            hosts_self: f.host_agents.contains(&a.pid) || pane_hosts,
        });
    }
    let force_level = if !opts.force || f.missing {
        0
    } else if f.locked.is_some() {
        2
    } else if !f.dirty.is_empty() {
        1
    } else {
        0
    };
    if f.listed {
        steps.push(Step::RemoveWorktree {
            repo: f.project.repo.clone(),
            path: f.path.clone(),
            restore: match &f.branch {
                Some(b) => Restore::Branch(b.clone()),
                None => Restore::Detached(f.head.clone()),
            },
            force: force_level,
            missing: f.missing,
        });
    }
    match (&f.branch, &f.keep_branch) {
        (Some(b), None) => steps.push(Step::DeleteBranch {
            repo: f.project.repo.clone(),
            branch: b.clone(),
            sha: f.head.clone(),
            upstream: f.upstream.clone(),
            best_effort: opts.gc,
        }),
        (Some(_), Some(why)) => notes.push(format!("branch kept: {why}")),
        (None, _) => {}
    }
    if f.upstream.is_some() || f.branch.is_some() {
        notes.push("remote branches are not touched".into());
    }
    for s in &f.sessions {
        if s.whole {
            steps.push(Step::KillSession {
                session: s.name.clone(),
                hosts_self: host_session(&s.name),
            });
        } else {
            for p in s.panes.iter().filter(|p| !agent_panes.contains(p)) {
                steps.push(Step::KillPane {
                    target: p.clone(),
                    hosts_self: f.host_panes.contains(p),
                });
            }
        }
    }
    if steps.iter().any(Step::hosts_self) {
        notes.push(
            "this fleet runs inside the task's tmux/agent: that one is killed last, after everything else"
                .into(),
        );
    }

    Ok(ClosePlan {
        target: Target {
            name: f.name.clone(),
            project: f.project.name.clone(),
            repo: f.project.repo.clone(),
            path: f.path.clone(),
            branch: f.branch.clone(),
            head: f.head.clone(),
            tasks: f.tasks.iter().map(|t| t.slug.clone()).collect(),
        },
        txn: Txn::new(format!("close {}", f.name), steps),
        blockers,
        notes,
        force: opts.force,
        facts: f.clone(),
    })
}

/// The refusal text for a blocked plan.
pub fn refusal(p: &ClosePlan) -> String {
    let mut s = format!("refused to close {} — this would be lost:\n", p.target.name);
    for l in p.blocker_lines(10) {
        s.push_str(&format!("  {l}\n"));
    }
    s.push_str("pass --force to close anyway");
    s
}

// ───────────────────────── apply ─────────────────────────

/// Re-read the world and re-plan; refuse when it no longer matches what was confirmed (an
/// agent started, a file changed), else run the transaction. `fleet` must be a fresh snapshot.
pub async fn apply(
    confirmed: &ClosePlan,
    fleet: &Fleet,
    opts: &CloseOpts,
    exec: &dyn Exec,
    log: &Progress,
) -> Result<TxnReport> {
    let r = Resolved {
        project: confirmed.facts.project.clone(),
        path: confirmed.target.path.clone(),
    };
    let fresh = plan(&gather(&r, fleet).await?, opts)?;
    if fresh.blocked() {
        bail!("{}", refusal(&fresh));
    }
    if fresh.txn.steps != confirmed.txn.steps {
        bail!(
            "{} changed since you confirmed (now: {}); nothing was done, run close again",
            confirmed.target.name,
            fresh
                .txn
                .steps
                .iter()
                .map(Step::describe)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(txn::apply(&fresh.txn, exec, log).await)
}

/// `apply` with the real executor.
pub async fn apply_real(
    confirmed: &ClosePlan,
    fleet: &Fleet,
    opts: &CloseOpts,
    log: &Progress,
) -> Result<TxnReport> {
    apply(confirmed, fleet, opts, &txn::Real, log).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pid: u32, pane: Option<&str>) -> AgentRow {
        AgentRow {
            adapter: "opencode".into(),
            pid,
            cwd: "/w/feat-x".into(),
            session_id: Some("ses_1".into()),
            title: None,
            mode: None,
            activity: Activity::Working,
            running_tool: None,
            plan_url: None,
            plan_title: None,
            port: None,
            last_activity: None,
            pane_target: pane.map(String::from),
            pending: vec![],
        }
    }

    fn clean() -> Facts {
        Facts {
            project: Project {
                name: "svc".into(),
                repo: "/r".into(),
                worktrees: None,
                reviews: None,
                base: "main".into(),
            },
            path: "/w/feat-x".into(),
            name: "feat-x".into(),
            is_main: false,
            listed: true,
            missing: false,
            branch: Some("feat/x".into()),
            head: "89aa2b0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            upstream: Some("origin/feat/x".into()),
            locked: None,
            dirty: vec![],
            only_here: vec![],
            only_here_more: false,
            review_merged: None,
            keep_branch: None,
            tasks: vec![TaskFacts {
                slug: "feat-x".into(),
                session: "feat-x".into(),
                symlinks: vec![("/w/wip-ab12".into(), "/w/feat-x".into())],
            }],
            agents: vec![],
            sessions: vec![SessionFacts {
                name: "feat-x".into(),
                whole: true,
                panes: vec!["feat-x:1.0".into()],
            }],
            host_panes: vec![],
            host_agents: vec![],
        }
    }

    fn kinds(p: &ClosePlan) -> Vec<&'static str> {
        p.txn
            .steps
            .iter()
            .map(|s| match s {
                Step::CloseTask { .. } => "close_task",
                Step::RemoveSymlink { .. } => "remove_symlink",
                Step::StopAgent { .. } => "stop_agent",
                Step::RemoveWorktree { .. } => "remove_worktree",
                Step::DeleteBranch { .. } => "delete_branch",
                Step::KillPane { .. } => "kill_pane",
                Step::KillSession { .. } => "kill_session",
            })
            .collect()
    }

    #[test]
    fn clean_task_plans_everything_in_order() {
        let p = plan(&clean(), &CloseOpts::default()).unwrap();
        assert!(p.blockers.is_empty() && !p.blocked());
        assert_eq!(
            kinds(&p),
            [
                "close_task",
                "remove_symlink",
                "remove_worktree",
                "delete_branch",
                "kill_session"
            ]
        );
        match &p.txn.steps[3] {
            Step::DeleteBranch {
                sha,
                upstream,
                best_effort,
                ..
            } => {
                assert!(sha.starts_with("89aa2b0"));
                assert_eq!(upstream.as_deref(), Some("origin/feat/x"));
                assert!(!best_effort);
            }
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn blockers_list_exactly_what_would_be_lost() {
        let mut f = clean();
        f.dirty = vec![" M src/a.rs".into(), "?? notes.md".into()];
        f.only_here = vec!["1a2b3c4 feat: x".into()];
        f.agents = vec![agent(4242, Some("feat-x:1.1"))];
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(p.blocked());
        assert_eq!(
            p.blockers,
            vec![
                Blocker::Dirty {
                    files: vec![" M src/a.rs".into(), "?? notes.md".into()]
                },
                Blocker::Unpushed {
                    commits: vec!["1a2b3c4 feat: x".into()],
                    more: false
                },
                Blocker::AgentRunning {
                    pid: 4242,
                    activity: Activity::Working,
                    pane: Some("feat-x:1.1".into()),
                    session_id: Some("ses_1".into())
                },
            ]
        );
        let text = refusal(&p);
        assert!(text.contains("src/a.rs") && text.contains("1a2b3c4") && text.contains("4242"));
        // Without force the worktree removal never drops files.
        assert!(p
            .txn
            .steps
            .iter()
            .any(|s| matches!(s, Step::RemoveWorktree { force: 0, .. })));
    }

    #[test]
    fn force_stops_agents_before_the_worktree_and_forces_removal() {
        let mut f = clean();
        f.dirty = vec!["?? x".into()];
        f.agents = vec![agent(4242, Some("feat-x:1.1"))];
        let p = plan(
            &f,
            &CloseOpts {
                force: true,
                gc: false,
            },
        )
        .unwrap();
        assert!(!p.blocked());
        assert_eq!(
            kinds(&p),
            [
                "close_task",
                "remove_symlink",
                "stop_agent",
                "remove_worktree",
                "delete_branch",
                "kill_session"
            ]
        );
        assert!(matches!(
            p.txn.steps[3],
            Step::RemoveWorktree { force: 1, .. }
        ));
        // The same plan forced from the popup's stored facts.
        let unforced = plan(&f, &CloseOpts::default()).unwrap();
        assert_eq!(unforced.forced(false).unwrap().txn, p.txn);
    }

    #[test]
    fn merged_review_turns_local_commits_into_a_note() {
        let mut f = clean();
        f.only_here = vec!["1a2b3c4 feat: x".into()];
        f.review_merged = Some("!514".into());
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(p.blockers.is_empty());
        assert!(p
            .notes
            .iter()
            .any(|n| n.contains("!514") && n.contains("1a2b3c4")));
    }

    #[test]
    fn main_clone_is_refused_even_forced() {
        let mut f = clean();
        f.is_main = true;
        assert!(plan(
            &f,
            &CloseOpts {
                force: true,
                gc: false
            }
        )
        .is_err());
    }

    #[test]
    fn kept_branch_has_no_delete_step() {
        let mut f = clean();
        f.keep_branch = Some("feat/x is also checked out in ~/y".into());
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(!kinds(&p).contains(&"delete_branch"));
        assert!(p.notes.iter().any(|n| n.contains("branch kept")));
    }

    #[test]
    fn detached_review_worktree_restores_by_sha() {
        let mut f = clean();
        f.branch = None;
        f.upstream = None;
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(!kinds(&p).contains(&"delete_branch"));
        assert!(p.txn.steps.iter().any(|s| matches!(
            s,
            Step::RemoveWorktree { restore: Restore::Detached(sha), .. } if sha.starts_with("89aa2b0")
        )));
    }

    #[test]
    fn missing_folder_skips_dirty_and_lock() {
        let mut f = clean();
        f.missing = true;
        f.dirty = vec!["?? x".into()]; // stale, cannot be read anyway
        f.locked = Some(String::new());
        f.sessions.clear();
        let p = plan(
            &f,
            &CloseOpts {
                force: true,
                gc: false,
            },
        )
        .unwrap();
        assert!(p.blockers.is_empty());
        assert!(p.txn.steps.iter().any(|s| matches!(
            s,
            Step::RemoveWorktree {
                missing: true,
                force: 0,
                ..
            }
        )));
    }

    #[test]
    fn rerun_after_a_half_finished_close_finishes_it() {
        let mut f = clean();
        f.listed = false;
        f.missing = true;
        f.sessions.clear();
        f.tasks[0].symlinks.clear();
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert_eq!(kinds(&p), ["close_task", "delete_branch"]);
        assert!(p.notes.iter().any(|n| n.contains("half-way")));
    }

    #[test]
    fn shared_session_loses_only_its_panes() {
        let mut f = clean();
        f.tasks.clear();
        f.sessions = vec![SessionFacts {
            name: "main".into(),
            whole: false,
            panes: vec!["main:2.0".into(), "main:2.1".into()],
        }];
        f.agents = vec![agent(7, Some("main:2.1"))];
        let p = plan(
            &f,
            &CloseOpts {
                force: true,
                gc: false,
            },
        )
        .unwrap();
        assert_eq!(
            kinds(&p),
            [
                "stop_agent",
                "remove_worktree",
                "delete_branch",
                "kill_pane"
            ]
        );
        // The agent's pane is stopped once, not killed twice.
        assert!(matches!(&p.txn.steps[3], Step::KillPane { target, .. } if target == "main:2.0"));
    }

    #[test]
    fn own_session_is_killed_last() {
        let mut f = clean();
        f.host_panes = vec!["feat-x:2.0".into()];
        f.sessions.push(SessionFacts {
            name: "other".into(),
            whole: true,
            panes: vec!["other:1.0".into()],
        });
        let p = plan(&f, &CloseOpts::default()).unwrap();
        let last = p.txn.steps.last().unwrap();
        assert!(
            matches!(last, Step::KillSession { session, hosts_self: true } if session == "feat-x")
        );
        assert!(matches!(
            &p.txn.steps[p.txn.steps.len() - 2],
            Step::KillSession { session, hosts_self: false } if session == "other"
        ));
    }

    #[test]
    fn hosting_agent_is_stopped_last() {
        let mut f = clean();
        f.agents = vec![agent(4242, Some("feat-x:1.1"))];
        f.host_agents = vec![4242];
        f.host_panes = vec!["feat-x:1.1".into()];
        let p = plan(
            &f,
            &CloseOpts {
                force: true,
                gc: false,
            },
        )
        .unwrap();
        let n = p.txn.steps.len();
        assert!(p.txn.steps[n - 2..].iter().all(Step::hosts_self));
        assert!(matches!(p.txn.steps[n - 3], Step::DeleteBranch { .. }));
    }

    #[test]
    fn locked_worktree_blocks_and_force_uses_double_force() {
        let mut f = clean();
        f.locked = Some("on usb".into());
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert_eq!(
            p.blockers,
            vec![Blocker::Locked {
                reason: "on usb".into()
            }]
        );
        let p = plan(
            &f,
            &CloseOpts {
                force: true,
                gc: false,
            },
        )
        .unwrap();
        assert!(p
            .txn
            .steps
            .iter()
            .any(|s| matches!(s, Step::RemoveWorktree { force: 2, .. })));
    }

    #[test]
    fn gc_mode_deletes_branch_best_effort() {
        let p = plan(
            &clean(),
            &CloseOpts {
                force: false,
                gc: true,
            },
        )
        .unwrap();
        assert!(p.txn.steps.iter().any(|s| matches!(
            s,
            Step::DeleteBranch {
                best_effort: true,
                ..
            }
        )));
    }

    fn fleet_with(wts: &[(&str, &str, &str)], panes: &[(&str, &str)]) -> Fleet {
        use crate::model::*;
        let row = |repo: &str, path: &str, branch: &str| WorktreeRow {
            repo: repo.into(),
            path: path.into(),
            name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into(),
            branch: Some(branch.into()),
            head: "abc".into(),
            upstream: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            detached: false,
            is_main: false,
            missing: false,
        };
        Fleet {
            worktrees: wts
                .iter()
                .map(|(r, p, b)| WorktreeView {
                    worktree: row(r, p, b),
                    overall: Overall::Dead,
                    agents: vec![],
                    panes: vec![],
                    review: None,
                    gc: GcVerdict::No("in use".into()),
                })
                .collect(),
            panes: panes
                .iter()
                .map(|(s, path)| PaneView {
                    pane: PaneRow {
                        session: (*s).into(),
                        window: 1,
                        window_name: "z".into(),
                        pane: 0,
                        pid: 1,
                        cwd: (*path).into(),
                        command: "zsh".into(),
                        title: String::new(),
                        attached: false,
                    },
                    worktree: wts
                        .iter()
                        .find(|(_, p, _)| p == path)
                        .map(|(r, p, b)| row(r, p, b)),
                    agent: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn task(slug: &str, repo: &str, path: &str) -> crate::registry::Task {
        crate::registry::Task {
            slug: slug.into(),
            kind: "start".into(),
            repo: repo.into(),
            branch: None,
            path: path.into(),
            old_paths: vec![],
            tmux_session: slug.into(),
            port: None,
            agent: "opencode".into(),
            mode: None,
            review_source: None,
            review_id: None,
            head_sha: None,
            first_prompt: None,
            created_at: chrono::Utc::now(),
            closed_at: None,
        }
    }

    #[test]
    fn resolve_by_slug_name_branch_session() {
        let f = fleet_with(
            &[
                ("svc", "/nonexistent/w/feat-x", "feat/x"),
                ("web", "/nonexistent/v/fix-y", "fix/y"),
            ],
            &[("sess-y", "/nonexistent/v/fix-y")],
        );
        let t = [task("lev8", "svc", "/nonexistent/w/feat-x")];
        let p = |s| resolve_name(&f, &t, s, None).unwrap();
        assert_eq!(p("lev8"), ("/nonexistent/w/feat-x".into(), "svc".into()));
        assert_eq!(p("feat-x").1, "svc");
        assert_eq!(p("fix/y").1, "web");
        assert_eq!(p("sess-y").1, "web");
        assert!(resolve_name(&f, &t, "nope", None)
            .unwrap_err()
            .to_string()
            .contains("no task"));
    }

    #[test]
    fn resolve_ambiguous_lists_both_and_scope_breaks_the_tie() {
        let f = fleet_with(
            &[
                ("svc", "/nonexistent/w/feat-x", "feat/x"),
                ("web", "/nonexistent/v/feat-x", "feat/x"),
            ],
            &[],
        );
        let e = resolve_name(&f, &[], "feat-x", None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("svc:") && e.contains("web:"), "{e}");
        assert_eq!(
            resolve_name(&f, &[], "feat-x", Some("web")).unwrap().1,
            "web"
        );
    }

    #[test]
    fn blocker_lines_truncate() {
        let b = Blocker::Dirty {
            files: (0..15).map(|i| format!("?? f{i}")).collect(),
        };
        let l = b.lines(10);
        assert_eq!(l.len(), 12);
        assert!(l.last().unwrap().contains("5 more"));
    }
}
