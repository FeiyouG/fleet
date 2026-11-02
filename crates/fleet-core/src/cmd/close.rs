//! `fleet worktree close TARGET` (alias `fleet close`): finish a task and remove everything
//! that belonged to it — agents, tmux panes/sessions, the worktree, its local branch, the
//! registry task and rename symlinks — all-or-nothing (see [`super::txn`]).
//!
//! resolve (name → worktree or leftovers) → gather (all reads) → plan (pure) → apply
//! (re-check, then run). A task whose worktree is already gone (deleted by hand, or a close
//! that stopped half-way) is still closable: whatever is left — branch, tmux, registry row —
//! is planned the same way.

use super::txn::{self, Exec, Restore, Step, Txn, TxnReport};
use super::Progress;
use crate::config::{expand, Config, Project};
use crate::model::{Activity, AgentRow, Fleet, ReviewRow, ReviewStatus};
use crate::procs::{canon, ProcTable};
use crate::registry::{Registry, Task};
use crate::reviews;
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

/// What a target resolved to: a worktree folder (listed by git, or already deleted), a
/// branch, or both.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub project: Project,
    /// The worktree folder; `None` for a bare leftover branch.
    pub path: Option<PathBuf>,
    /// The branch, when it is known without a listed worktree (leftover branch).
    pub branch: Option<String>,
}

impl Resolved {
    pub fn worktree(project: Project, path: PathBuf) -> Self {
        Self {
            project,
            path: Some(path),
            branch: None,
        }
    }
}

/// Something that would be lost; `--force` closes anyway.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Blocker {
    /// `git status --porcelain` lines.
    Dirty {
        files: Vec<String>,
    },
    /// Commits on no live remote branch, tag, other local branch or merged review head.
    Unpushed {
        commits: Vec<String>,
        more: bool,
    },
    AgentRunning {
        adapter: String,
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
                    "commits only on this branch ({}{}), on no remote branch, tag, other branch or merged review:",
                    commits.len(),
                    if *more { "+" } else { "" }
                ),
                commits,
                *more,
            ),
            Blocker::AgentRunning {
                adapter,
                pid,
                activity,
                pane,
                session_id,
            } => vec![format!(
                "agent running: {adapter} pid {pid} {}{}{}",
                activity.label(),
                pane.as_deref()
                    .map(|p| format!(" in {p}"))
                    .unwrap_or_default(),
                txn::resume_hint(adapter, session_id.as_deref())
                    .map(|h| format!(" (resume later: {h})"))
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
    /// The worktree folder; `None` for a leftover branch with no folder known.
    pub path: Option<PathBuf>,
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
    /// The worktree folder (may no longer exist); `None` for a bare leftover branch.
    pub path: Option<PathBuf>,
    pub name: String,
    pub is_main: bool,
    /// git lists the worktree. False: it was removed already (by hand, or by a close that
    /// stopped half-way) and only leftovers (branch, tmux, registry) remain.
    pub listed: bool,
    /// The folder does not exist.
    pub missing: bool,
    pub branch: Option<String>,
    /// The branch was found by the folder name (no registry row said so).
    pub branch_guessed: bool,
    /// Full sha.
    pub head: String,
    pub upstream: Option<String>,
    pub locked: Option<String>,
    pub dirty: Vec<String>,
    /// Commits that deleting the branch would lose (blockers).
    pub only_here: Vec<String>,
    pub only_here_more: bool,
    /// Commits only on this branch that a merged review contains (squash/rebase merge).
    pub landed: Vec<String>,
    /// Display id of a merged review for this branch (`!514`).
    pub review_merged: Option<String>,
    /// Why the branch must stay (base branch, checked out elsewhere).
    pub keep_branch: Option<String>,
    pub tasks: Vec<TaskFacts>,
    pub agents: Vec<AgentRow>,
    pub sessions: Vec<SessionFacts>,
    /// Pane targets / agent pids that are ancestors of this `fleet` process.
    pub host_panes: Vec<String>,
    pub host_agents: Vec<u32>,
    /// Things gather could not check (remote unreachable, forge lookup failed).
    pub notes: Vec<String>,
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
        let wt = match (&t.path, self.facts.listed) {
            (Some(p), true) => txn::tilde(p),
            (Some(p), false) => format!("{} (worktree already gone)", txn::tilde(p)),
            (None, _) => "no worktree".into(),
        };
        format!(
            "close {} ({}{}) · {} on {}",
            t.name,
            t.project,
            if t.tasks.is_empty() {
                ""
            } else {
                ", fleet task"
            },
            wt,
            t.branch.clone().unwrap_or_else(|| if t.head.is_empty() {
                "no branch".into()
            } else {
                format!("detached {}", &t.head[..t.head.len().min(8)])
            })
        )
    }

    /// How `apply` finds the same thing again.
    pub fn resolved(&self) -> Resolved {
        Resolved {
            project: self.facts.project.clone(),
            path: self.facts.path.clone(),
            branch: if self.facts.listed {
                None
            } else {
                self.facts.branch.clone()
            },
        }
    }
}

// ───────────────────────── resolve ─────────────────────────

fn looks_like_path(t: &str) -> bool {
    t.contains('/') || t == "." || t == ".." || t.starts_with('~')
}

/// Unmistakably a path (`/…`, `./…`, `../…`, `~…`), never a branch name.
fn explicit_path(t: &str) -> bool {
    t.starts_with('/') || t.starts_with('.') || t.starts_with('~')
}

/// Canonicalise the longest existing ancestor and re-append the rest, so a deleted folder
/// compares equal however it was spelled (`/tmp` vs `/private/tmp`).
pub fn canon_lossy(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(leaf)) if parent != p => canon_lossy(parent).join(leaf),
        _ => p.to_path_buf(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || canon_lossy(a) == canon_lossy(b)
}

fn under(p: &Path, root: &Path) -> bool {
    p.starts_with(root) || canon_lossy(p).starts_with(canon_lossy(root))
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

/// The project a (possibly deleted) folder belonged to: under its repo or its worktrees dir,
/// else the only configured project.
fn project_for_dir<'a>(projects: &'a [Project], dir: &Path) -> Option<&'a Project> {
    projects
        .iter()
        .find(|p| under(dir, &p.repo) || under(dir, &p.worktrees_dir()))
        .or(if projects.len() == 1 {
            projects.first()
        } else {
            None
        })
}

/// A worktree or set of leftovers a name can mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub project: Option<String>,
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    /// git lists `path` as a worktree.
    pub listed: bool,
}

/// Find what `target` names. A path: any folder inside a worktree, a rename symlink, or a
/// deleted worktree folder (git, the registry or a tmux pane still knows it). Otherwise a
/// registry task slug, worktree folder name, branch, or tmux session; when none of those
/// names a listed worktree, leftovers: a local branch checked out nowhere, or tmux panes
/// whose folder is gone. `scope` (a project name) only breaks ties.
pub async fn resolve(
    fleet: &Fleet,
    cfg: &Config,
    target: &str,
    scope: Option<&str>,
) -> Result<Resolved> {
    let tasks = Registry::open()
        .and_then(|r| r.open_tasks())
        .unwrap_or_default();
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
            return Ok(Resolved::worktree(project_for_main(cfg, &main), top));
        }
        // A deleted folder: git's list, the registry, or a tmux pane still knows it.
        if let Some(w) = fleet
            .worktrees
            .iter()
            .find(|w| same_path(&w.worktree.path, &p))
        {
            let project = cfg
                .project(&w.worktree.repo)
                .cloned()
                .context("project not in config")?;
            return Ok(Resolved::worktree(project, w.worktree.path.clone()));
        }
        let known = tasks
            .iter()
            .find(|t| same_path(&t.path, &p))
            .map(|t| t.repo.clone());
        let pane = fleet.panes.iter().any(|v| under(&v.pane.cwd, &p));
        let project = match known {
            Some(repo) => cfg.project(&repo).cloned(),
            None if pane => project_for_dir(&cfg.projects, &p).cloned(),
            None => None,
        };
        match project {
            Some(project) => {
                return Ok(Resolved {
                    project,
                    path: Some(p),
                    branch: None,
                })
            }
            // `refactor/x` is a branch name more often than a relative path.
            None if !explicit_path(target) => {}
            None => bail!("{} does not exist and is no known worktree", p.display()),
        }
    }

    let mut loose = vec![];
    for p in &cfg.projects {
        for b in git::loose_branches_named(&p.repo, target)
            .await
            .unwrap_or_default()
        {
            loose.push((p.name.clone(), b));
        }
    }
    let c = resolve_name(fleet, &tasks, &cfg.projects, &loose, target, scope)?;
    let project = match c.project.as_deref().and_then(|n| cfg.project(n)) {
        Some(p) => p.clone(),
        None => {
            let dir = c
                .path
                .clone()
                .context("cannot tell which project this belongs to; pass a path")?;
            let main = git::main_clone(&dir)
                .await
                .context("cannot tell which project this belongs to; pass --project")?;
            project_for_main(cfg, &main)
        }
    };
    Ok(Resolved {
        project,
        path: c.path,
        branch: if c.listed { None } else { c.branch },
    })
}

/// Name lookup (pure). Tier 1: registry slugs, listed worktree names and branches, tmux
/// sessions on a listed worktree. Tier 2, only when tier 1 finds nothing: leftovers — tmux
/// sessions (or panes whose folder is named `target`) whose folder is gone, and `loose`
/// branches `(project, branch)` checked out nowhere. Leftovers of one task (its old folder
/// and its branch) merge into one candidate. Several remaining candidates are an error
/// unless `scope` picks one.
pub fn resolve_name(
    fleet: &Fleet,
    tasks: &[Task],
    projects: &[Project],
    loose: &[(String, String)],
    target: &str,
    scope: Option<&str>,
) -> Result<Candidate> {
    let mut cands: Vec<Candidate> = vec![];
    let add = |c: Candidate, cands: &mut Vec<Candidate>| {
        if !cands.iter().any(|x| {
            x.project == c.project
                && match (&x.path, &c.path) {
                    (Some(a), Some(b)) => same_path(a, b),
                    _ => x.branch.is_some() && x.branch == c.branch,
                }
        }) {
            cands.push(c);
        }
    };
    let listed_path = |p: &Path| {
        fleet
            .worktrees
            .iter()
            .any(|w| same_path(&w.worktree.path, p))
    };
    for t in tasks.iter().filter(|t| t.slug == target) {
        add(
            Candidate {
                project: Some(t.repo.clone()),
                path: Some(canon_lossy(&t.path)),
                branch: t.branch.clone(),
                listed: listed_path(&t.path),
            },
            &mut cands,
        );
    }
    for w in &fleet.worktrees {
        let w = &w.worktree;
        if w.name == target || w.branch.as_deref() == Some(target) {
            add(
                Candidate {
                    project: Some(w.repo.clone()),
                    path: Some(canon_lossy(&w.path)),
                    branch: w.branch.clone(),
                    listed: true,
                },
                &mut cands,
            );
        }
    }
    for p in &fleet.panes {
        if p.pane.session == target {
            if let Some(w) = &p.worktree {
                add(
                    Candidate {
                        project: Some(w.repo.clone()),
                        path: Some(canon_lossy(&w.path)),
                        branch: w.branch.clone(),
                        listed: true,
                    },
                    &mut cands,
                );
            }
        }
    }

    if cands.is_empty() {
        // Leftovers: panes in a vanished folder (by session name or folder name) …
        let mut gone: Vec<Candidate> = vec![];
        for p in &fleet.panes {
            let cwd = &p.pane.cwd;
            let leaf = cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            if p.worktree.is_none()
                && !cwd.exists()
                && (p.pane.session == target || leaf.as_deref() == Some(target))
            {
                let c = Candidate {
                    project: project_for_dir(projects, cwd).map(|x| x.name.clone()),
                    path: Some(canon_lossy(cwd)),
                    branch: None,
                    listed: false,
                };
                if !gone.iter().any(|g| g.path == c.path) {
                    gone.push(c);
                }
            }
        }
        // … and local branches checked out nowhere.
        let mut branches: Vec<Candidate> = loose
            .iter()
            .map(|(proj, b)| Candidate {
                project: Some(proj.clone()),
                path: None,
                branch: Some(b.clone()),
                listed: false,
            })
            .collect();
        // One vanished folder + one branch whose last segment is the folder name = one task.
        for g in &mut gone {
            let leaf = g
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let same: Vec<usize> = branches
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    (g.project.is_none() || g.project == b.project)
                        && b.branch
                            .as_deref()
                            .map(|x| x == leaf || x.ends_with(&format!("/{leaf}")))
                            .unwrap_or(false)
                })
                .map(|(i, _)| i)
                .collect();
            if let [i] = same[..] {
                let b = branches.remove(i);
                g.project = g.project.take().or(b.project);
                g.branch = b.branch;
            }
        }
        for c in gone.into_iter().chain(branches) {
            add(c, &mut cands);
        }
    }

    if cands.len() > 1 {
        if let Some(s) = scope {
            cands.retain(|c| c.project.as_deref() == Some(s));
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
                .map(|c| {
                    format!(
                        "{}: {}",
                        c.project.as_deref().unwrap_or("?"),
                        match (&c.path, &c.branch) {
                            (Some(p), _) => txn::tilde(p),
                            (None, Some(b)) => format!("branch {b}"),
                            (None, None) => "?".into(),
                        }
                    )
                })
                .collect();
            bail!(
                "`{target}` matches several ({}); pass a path or --project",
                all.join(", ")
            )
        }
    })
}

// ───────────────────────── gather ─────────────────────────

const LIMIT: usize = 20;

/// Read everything about the resolved target: git, registry, forge (merged review), the
/// snapshot (agents, panes), and which pane/agent hosts this process.
pub async fn gather(r: &Resolved, fleet: &Fleet) -> Result<Facts> {
    let repo = &r.project.repo;
    let entries = git::porcelain(repo).await?;
    let idx = r
        .path
        .as_ref()
        .and_then(|p| entries.iter().position(|e| same_path(&e.path, p)));
    let reg_tasks: Vec<Task> = match &r.path {
        Some(p) => Registry::open()
            .and_then(|reg| reg.tasks_for_path(p))
            .unwrap_or_default(),
        None => vec![],
    };
    let mut notes = vec![];
    let listed = idx.is_some();

    // ── the worktree entry, or what is left of it ──
    let (path, branch, head, branch_guessed, locked, bare) = match idx {
        Some(i) => {
            let e = &entries[i];
            let missing = e.prunable || !e.path.exists();
            let path = if missing {
                e.path.clone()
            } else {
                canon(&e.path)
            };
            (
                Some(path),
                e.branch.clone(),
                e.head.clone(),
                false,
                e.locked.clone(),
                e.bare,
            )
        }
        None => {
            if let Some(p) = &r.path {
                if p.exists() {
                    bail!("{} is not a worktree of {}", p.display(), repo.display());
                }
            }
            // Which branch: given, from the registry, else the one named like the folder.
            let mut reg_branch = None;
            for b in reg_tasks.iter().filter_map(|t| t.branch.as_ref()) {
                if git::rev_parse(repo, &format!("refs/heads/{b}"))
                    .await
                    .is_some()
                {
                    reg_branch = Some(b.clone());
                    break;
                }
            }
            // A branch tied to a deleted folder by name only (resolve matched them) is a guess.
            let mut guessed = r.branch.is_some() && r.path.is_some() && r.branch != reg_branch;
            let mut branch = r.branch.clone().or(reg_branch);
            if branch.is_none() {
                if let Some(leaf) = r.path.as_ref().and_then(|p| p.file_name()) {
                    let found = git::loose_branches_named(repo, &leaf.to_string_lossy())
                        .await
                        .unwrap_or_default();
                    if let [b] = &found[..] {
                        branch = Some(b.clone());
                        guessed = true;
                    } else if found.len() > 1 {
                        notes.push(format!(
                            "several branches are named like the folder ({}); none deleted — close each by name",
                            found.join(", ")
                        ));
                    }
                }
            }
            let head = match &branch {
                Some(b) => git::rev_parse(repo, &format!("refs/heads/{b}"))
                    .await
                    .with_context(|| format!("no local branch {b}"))?,
                None => String::new(),
            };
            if let Some(b) = &branch {
                if let Some(e) = entries.iter().find(|e| e.branch.as_deref() == Some(b)) {
                    bail!(
                        "branch {b} is checked out in {}; close that worktree instead",
                        txn::tilde(&e.path)
                    );
                }
            }
            (r.path.clone(), branch, head, guessed, None, false)
        }
    };
    let missing = path.as_ref().map(|p| !p.exists()).unwrap_or(true);
    let name = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| branch.clone())
        .unwrap_or_default();
    let is_main = idx == Some(0) && !bare;

    let upstream = match &branch {
        Some(b) => git::git(
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
        None => None,
    };
    let dirty = match (&path, listed && !missing && !is_main) {
        (Some(p), true) => git::status_lines(p).await?,
        _ => vec![],
    };

    // ── merged review: the snapshot (open + per-branch merged lookups), else ask the forge ──
    let mut merged: Option<ReviewRow> = path.as_ref().and_then(|p| {
        fleet
            .worktrees
            .iter()
            .filter(|w| same_path(&w.worktree.path, p))
            .filter_map(|w| w.review.clone())
            .find(|r| r.status == ReviewStatus::Merged)
    });
    if merged.is_none() && !is_main {
        if let (Some(b), Some(src)) = (&branch, &r.project.reviews) {
            match reviews::adapter_for(src) {
                Ok(a) => match a.merged_for_branch(b).await {
                    Ok(m) => merged = m,
                    Err(e) => {
                        notes.push(format!("could not ask {src} whether {b} was merged: {e:#}"))
                    }
                },
                Err(e) => notes.push(format!("{e:#}")),
            }
        }
    }

    // ── what deleting the branch would lose ──
    let (mut only_here, landed) = if head.is_empty() || is_main {
        (vec![], vec![])
    } else {
        let anchors = match git::live_remote_tips(repo).await {
            Ok(t) => git::RemoteAnchors::Live(t),
            Err(e) => {
                notes.push(format!(
                    "remote unreachable ({e:#}); trusting remote-tracking refs from the last fetch"
                ));
                git::RemoteAnchors::TrackingRefs
            }
        };
        let all = git::commits_only_here(repo, &head, branch.as_deref(), &anchors, &[], LIMIT + 1)
            .await?;
        match &merged {
            Some(m) if !all.is_empty() => {
                let heads = git::existing_commits(repo, std::slice::from_ref(&m.head_sha))
                    .await
                    .unwrap_or_default();
                if heads.is_empty() {
                    notes.push(format!(
                        "{} is merged, but its head {} is not in this clone, so fleet cannot tell whether the commits below are in it; `git fetch` and retry",
                        m.display_id,
                        &m.head_sha[..m.head_sha.len().min(8)]
                    ));
                    (all, vec![])
                } else {
                    let after = git::commits_only_here(
                        repo,
                        &head,
                        branch.as_deref(),
                        &anchors,
                        &heads,
                        LIMIT + 1,
                    )
                    .await?;
                    let landed = all.into_iter().filter(|c| !after.contains(c)).collect();
                    (after, landed)
                }
            }
            _ => (all, vec![]),
        }
    };
    let only_here_more = only_here.len() > LIMIT;
    only_here.truncate(LIMIT);

    let keep_branch = branch.as_ref().and_then(|b| {
        if *b == r.project.base {
            return Some(format!("{b} is the project's base branch"));
        }
        entries
            .iter()
            .enumerate()
            .find(|(i, x)| Some(*i) != idx && x.branch.as_deref() == Some(b.as_str()))
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

    // Inside this worktree (a deleted folder included: tmux keeps reporting it) and not
    // inside a nested, longer one.
    let others: Vec<PathBuf> = match &path {
        Some(p) => entries
            .iter()
            .enumerate()
            .filter(|(i, x)| Some(*i) != idx && !x.prunable)
            .map(|(_, x)| canon(&x.path))
            .filter(|x| under(x, p) && !same_path(x, p))
            .collect(),
        None => vec![],
    };
    let inside = |c: &Path| -> bool {
        match &path {
            Some(p) => !is_main && under(c, p) && !others.iter().any(|o| under(c, o)),
            None => false,
        }
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
        if inside(&v.pane.cwd) {
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

    if !listed && branch.is_none() && tasks.is_empty() && sessions.is_empty() && agents.is_empty() {
        bail!(
            "nothing left to close for {}",
            path.as_ref().map(|p| txn::tilde(p)).unwrap_or(name)
        );
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
        is_main,
        listed,
        missing,
        branch,
        branch_guessed,
        head,
        upstream,
        locked,
        dirty,
        only_here,
        only_here_more,
        landed,
        review_merged: merged.map(|m| m.display_id),
        keep_branch,
        tasks,
        agents,
        sessions,
        host_panes,
        host_agents,
        notes,
    })
}

// ───────────────────────── plan (pure) ─────────────────────────

pub fn plan(f: &Facts, opts: &CloseOpts) -> Result<ClosePlan> {
    let where_ = f
        .path
        .as_ref()
        .map(|p| txn::tilde(p))
        .unwrap_or_else(|| f.name.clone());
    if f.is_main {
        bail!(
            "{where_} is the main clone of {}; close removes worktrees, never the repo itself",
            f.project.name
        );
    }
    let mut blockers = vec![];
    let mut notes = f.notes.clone();
    if !f.missing && !f.dirty.is_empty() {
        blockers.push(Blocker::Dirty {
            files: f.dirty.clone(),
        });
    }
    if !f.only_here.is_empty() {
        blockers.push(Blocker::Unpushed {
            commits: f.only_here.clone(),
            more: f.only_here_more,
        });
    }
    if let Some(id) = &f.review_merged {
        if f.landed.is_empty() {
            notes.push(format!("{id} is merged"));
        } else {
            notes.push(format!(
                "{id} is merged and contains the {} commit(s) found only on this branch (squash/rebase merge): {}",
                f.landed.len(),
                f.landed.join(" · ")
            ));
        }
    }
    for a in &f.agents {
        blockers.push(Blocker::AgentRunning {
            adapter: a.adapter.clone(),
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
        notes.push(match &f.path {
            Some(_) => "git no longer lists this worktree (deleted by hand, or an earlier close stopped half-way): closing what is left".into(),
            None => "no worktree: closing the leftover branch".into(),
        });
    } else if f.missing {
        notes.push("the folder is already gone; only git's entry is removed".into());
    }
    if f.branch_guessed {
        if let Some(b) = &f.branch {
            notes.push(format!(
                "branch {b} found by the folder name (it is checked out nowhere)"
            ));
        }
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
            adapter: a.adapter.clone(),
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
    if let (true, Some(path)) = (f.listed, &f.path) {
        steps.push(Step::RemoveWorktree {
            repo: f.project.repo.clone(),
            path: path.clone(),
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
    if f.branch.is_some() {
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
    let fresh = plan(&gather(&confirmed.resolved(), fleet).await?, opts)?;
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
            path: Some("/w/feat-x".into()),
            name: "feat-x".into(),
            is_main: false,
            listed: true,
            missing: false,
            branch: Some("feat/x".into()),
            branch_guessed: false,
            head: "89aa2b0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            upstream: Some("origin/feat/x".into()),
            locked: None,
            dirty: vec![],
            only_here: vec![],
            only_here_more: false,
            landed: vec![],
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
            notes: vec![],
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
                    adapter: "opencode".into(),
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
    fn merged_review_containing_the_commits_is_a_note_not_a_blocker() {
        // Squash-merged on the forge, remote branch deleted and pruned: gather found the
        // commits on no live remote, but all inside the merged review's head.
        let mut f = clean();
        f.upstream = None;
        f.landed = vec!["1a2b3c4 feat: x".into()];
        f.review_merged = Some("!514".into());
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(p.blockers.is_empty());
        assert!(p
            .notes
            .iter()
            .any(|n| n.contains("!514") && n.contains("1a2b3c4")));
        assert!(kinds(&p).contains(&"delete_branch"));
    }

    #[test]
    fn commits_after_the_merge_still_block() {
        let mut f = clean();
        f.landed = vec!["1a2b3c4 feat: x".into()];
        f.only_here = vec!["9f9f9f9 wip: after merge".into()];
        f.review_merged = Some("!514".into());
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert_eq!(
            p.blockers,
            vec![Blocker::Unpushed {
                commits: vec!["9f9f9f9 wip: after merge".into()],
                more: false
            }]
        );
    }

    #[test]
    fn gather_notes_are_shown() {
        let mut f = clean();
        f.notes = vec!["remote unreachable (x); trusting remote-tracking refs".into()];
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(p.notes.iter().any(|n| n.starts_with("remote unreachable")));
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
    fn worktree_gone_by_hand_leftover_branch_and_session_close() {
        // The internal-api-deno case: folder and git entry gone, no registry row, but the
        // branch (found by the folder name) and a 2-pane tmux session remain.
        let mut f = clean();
        f.listed = false;
        f.missing = true;
        f.path = Some("/w/internal-api-deno".into());
        f.name = "internal-api-deno".into();
        f.branch = Some("refactor/internal-api-deno".into());
        f.branch_guessed = true;
        f.upstream = None;
        f.tasks.clear();
        f.sessions = vec![SessionFacts {
            name: "monid-internal-api-deno-update".into(),
            whole: true,
            panes: vec![
                "monid-internal-api-deno-update:1.0".into(),
                "monid-internal-api-deno-update:1.1".into(),
            ],
        }];
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert!(!p.blocked());
        assert_eq!(kinds(&p), ["delete_branch", "kill_session"]);
        assert!(p
            .notes
            .iter()
            .any(|n| n.contains("found by the folder name")));
        assert!(p.header().contains("worktree already gone"));
    }

    #[test]
    fn bare_leftover_branch_closes() {
        let mut f = clean();
        f.listed = false;
        f.missing = true;
        f.path = None;
        f.name = "feat/x".into();
        f.tasks.clear();
        f.sessions.clear();
        let p = plan(&f, &CloseOpts::default()).unwrap();
        assert_eq!(kinds(&p), ["delete_branch"]);
        assert!(p.header().contains("no worktree"));
        let r = p.resolved();
        assert_eq!(r.path, None);
        assert_eq!(r.branch.as_deref(), Some("feat/x"));
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

    fn proj(name: &str, repo: &str, wts: &str) -> Project {
        Project {
            name: name.into(),
            repo: repo.into(),
            worktrees: Some(wts.into()),
            reviews: None,
            base: "main".into(),
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
        let p = |s| resolve_name(&f, &t, &[], &[], s, None).unwrap();
        let c = p("lev8");
        assert_eq!(c.path, Some("/nonexistent/w/feat-x".into()));
        assert_eq!(c.project.as_deref(), Some("svc"));
        assert!(c.listed);
        assert_eq!(p("feat-x").project.as_deref(), Some("svc"));
        assert_eq!(p("fix/y").project.as_deref(), Some("web"));
        assert_eq!(p("sess-y").project.as_deref(), Some("web"));
        assert!(resolve_name(&f, &t, &[], &[], "nope", None)
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
        let e = resolve_name(&f, &[], &[], &[], "feat-x", None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("svc:") && e.contains("web:"), "{e}");
        assert_eq!(
            resolve_name(&f, &[], &[], &[], "feat-x", Some("web"))
                .unwrap()
                .project
                .as_deref(),
            Some("web")
        );
    }

    #[test]
    fn resolve_leftovers_by_folder_branch_and_session() {
        // Worktree folder and git entry gone; a session with a pane still in the old folder
        // (no worktree joined), and the branch checked out nowhere.
        let projects = [
            proj("svc", "/nonexistent/repo", "/nonexistent/wt"),
            proj("web", "/nonexistent/web", "/nonexistent/wt-web"),
        ];
        let f = fleet_with(
            &[],
            &[(
                "monid-internal-api-deno-update",
                "/nonexistent/wt/internal-api-deno",
            )],
        );
        let loose = [("svc".to_string(), "refactor/internal-api-deno".to_string())];
        for target in ["internal-api-deno", "monid-internal-api-deno-update"] {
            let c = resolve_name(&f, &[], &projects, &loose, target, None).unwrap();
            assert_eq!(c.project.as_deref(), Some("svc"), "{target}");
            assert_eq!(
                c.path,
                Some("/nonexistent/wt/internal-api-deno".into()),
                "{target}"
            );
            assert!(!c.listed);
        }
        // By the branch alone (no session involved: the branch name does not match a folder).
        let c = resolve_name(
            &fleet_with(&[], &[]),
            &[],
            &projects,
            &loose,
            "refactor/internal-api-deno",
            None,
        )
        .unwrap();
        assert_eq!(c.branch.as_deref(), Some("refactor/internal-api-deno"));
        assert_eq!(c.path, None);
        // A listed worktree named like the target wins over leftovers.
        let f2 = fleet_with(
            &[("web", "/nonexistent/wt-web/internal-api-deno", "x/y")],
            &[],
        );
        let c = resolve_name(&f2, &[], &projects, &loose, "internal-api-deno", None).unwrap();
        assert!(c.listed && c.project.as_deref() == Some("web"));
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
