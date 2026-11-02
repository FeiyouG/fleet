//! Join the collector rows into the views the tabs render. Local-first: the Overview
//! starts from worktrees on this machine.

use crate::model::*;
use crate::procs::{canon, ProcTable};
use crate::status;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Inputs<'a> {
    pub panes: Vec<PaneRow>,
    pub agents: Vec<AgentRow>,
    pub worktrees: Vec<WorktreeRow>,
    pub reviews: Vec<ReviewRow>,
    pub procs: Option<&'a ProcTable>,
    /// repo name -> review source (`gitlab:group/project`), from config.
    pub repo_sources: HashMap<String, String>,
}

/// The worktree whose path is the longest prefix of `p`.
fn worktree_for<'a>(p: &Path, wts: &'a [WorktreeRow]) -> Option<&'a WorktreeRow> {
    wts.iter()
        .filter(|w| !w.missing && p.starts_with(&w.path))
        .max_by_key(|w| w.path.as_os_str().len())
}

pub fn join(inp: Inputs<'_>) -> Fleet {
    let Inputs {
        panes,
        agents,
        worktrees,
        reviews,
        procs,
        repo_sources,
    } = inp;

    // pane shell pid -> pane index
    let pane_by_pid: HashMap<u32, usize> =
        panes.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();

    // agent pid -> pane index (walk the process tree; fall back to cwd match)
    let mut agent_pane: HashMap<u32, usize> = HashMap::new();
    for a in &agents {
        let idx = procs
            .and_then(|t| t.ancestor_in(a.pid, &pane_by_pid))
            .or_else(|| {
                panes
                    .iter()
                    .position(|p| p.command == "opencode" && canon(&p.cwd) == a.cwd)
            });
        if let Some(i) = idx {
            agent_pane.insert(a.pid, i);
        }
    }

    // Stamp each agent with its pane target.
    let agents: Vec<AgentRow> = agents
        .into_iter()
        .map(|mut a| {
            a.pane_target = agent_pane.get(&a.pid).map(|&i| panes[i].target());
            a
        })
        .collect();

    // reviews by source branch
    let mut review_by_branch: HashMap<&str, Vec<&ReviewRow>> = HashMap::new();
    for r in &reviews {
        review_by_branch
            .entry(r.source_branch.as_str())
            .or_default()
            .push(r);
    }

    // ─── Overview + Worktrees ───
    let mut overview = Vec::new();
    let mut worktree_views = Vec::new();
    let mut claimed_agents: Vec<bool> = vec![false; agents.len()];
    let mut claimed_panes: Vec<bool> = vec![false; panes.len()];

    for w in &worktrees {
        let ags: Vec<AgentRow> = agents
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                worktree_for(&a.cwd, &worktrees)
                    .map(|x| x.path == w.path)
                    .unwrap_or(false)
            })
            .map(|(i, a)| {
                claimed_agents[i] = true;
                a.clone()
            })
            .collect();
        let pns: Vec<PaneRow> = panes
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                worktree_for(&canon(&p.cwd), &worktrees)
                    .map(|x| x.path == w.path)
                    .unwrap_or(false)
            })
            .map(|(i, p)| {
                claimed_panes[i] = true;
                p.clone()
            })
            .collect();
        // Reviews on this worktree's branch; for a detached `review-N` checkout, the review
        // whose head it sits at (or whose id its name carries, once the MR moved on).
        let rvs: Vec<ReviewRow> = match w.branch.as_deref() {
            Some(b) => review_by_branch
                .get(b)
                .map(|v| v.iter().map(|r| (*r).clone()).collect())
                .unwrap_or_default(),
            None => {
                let by_name = w
                    .name
                    .strip_prefix("review-")
                    .and_then(|n| n.parse::<u64>().ok());
                reviews
                    .iter()
                    .filter(|r| {
                        (w.head.len() >= 7 && r.head_sha.starts_with(&w.head))
                            || (by_name == Some(r.id)
                                && repo_sources.get(&w.repo) == Some(&r.source))
                    })
                    .cloned()
                    .collect()
            }
        };
        let primary_review = rvs.iter().find(|r| !r.status.is_done()).or(rvs.first());
        let last_activity = ags.iter().filter_map(|a| a.last_activity).max();
        overview.push(OverviewRow {
            overall: status::overall(Some(w), &ags, primary_review),
            worktree: Some(w.clone()),
            dir: w.path.clone(),
            agents: sort_agents(ags.clone()),
            panes: pns.clone(),
            reviews: rvs.clone(),
            last_activity,
        });
        worktree_views.push(WorktreeView {
            overall: status::overall(Some(w), &ags, primary_review),
            gc: status::gc_verdict(w, &ags, primary_review),
            worktree: w.clone(),
            agents: sort_agents(ags),
            panes: pns,
            review: primary_review.cloned(),
        });
    }

    // Agents outside every configured repo: group by cwd.
    let mut outside: HashMap<PathBuf, Vec<AgentRow>> = HashMap::new();
    for (i, a) in agents.iter().enumerate() {
        if !claimed_agents[i] {
            outside.entry(a.cwd.clone()).or_default().push(a.clone());
        }
    }
    let mut outside: Vec<_> = outside.into_iter().collect();
    outside.sort_by(|a, b| a.0.cmp(&b.0));
    for (dir, ags) in outside {
        let pns: Vec<PaneRow> = ags
            .iter()
            .filter_map(|a| agent_pane.get(&a.pid).map(|&i| panes[i].clone()))
            .collect();
        let last_activity = ags.iter().filter_map(|a| a.last_activity).max();
        overview.push(OverviewRow {
            overall: status::overall(None, &ags, None),
            worktree: None,
            dir,
            agents: sort_agents(ags),
            panes: pns,
            reviews: vec![],
            last_activity,
        });
    }

    overview.sort_by(|a, b| {
        a.worktree
            .is_none()
            .cmp(&b.worktree.is_none())
            .then(a.overall.cmp(&b.overall))
            .then(b.last_activity.cmp(&a.last_activity))
            .then(a.name().cmp(&b.name()))
    });

    // ─── Panes ───
    let pane_views: Vec<PaneView> = panes
        .iter()
        .enumerate()
        .map(|(i, p)| PaneView {
            pane: p.clone(),
            worktree: worktree_for(&canon(&p.cwd), &worktrees).cloned(),
            agent: agents
                .iter()
                .find(|a| agent_pane.get(&a.pid) == Some(&i))
                .cloned(),
        })
        .collect();

    // ─── Reviews ───
    let review_views: Vec<ReviewView> = reviews
        .iter()
        .map(|r| {
            let wts: Vec<WorktreeRow> = worktrees
                .iter()
                .filter(|w| {
                    // Same branch, or a detached checkout of the review head (`review-N`).
                    // A *branch* that merely shares the commit is not that review's worktree.
                    w.branch.as_deref() == Some(r.source_branch.as_str())
                        || (w.detached && w.head.len() >= 7 && r.head_sha.starts_with(&w.head))
                })
                .cloned()
                .collect();
            let ags: Vec<AgentRow> = agents
                .iter()
                .filter(|a| wts.iter().any(|w| a.cwd.starts_with(&w.path)))
                .cloned()
                .collect();
            ReviewView {
                review: r.clone(),
                worktrees: wts,
                agents: sort_agents(ags),
            }
        })
        .collect();

    Fleet {
        taken_at: Some(chrono::Utc::now()),
        reviews_fetched_at: None,
        overview,
        panes: pane_views,
        worktrees: worktree_views,
        reviews: review_views,
        warnings: vec![],
    }
}

fn sort_agents(mut v: Vec<AgentRow>) -> Vec<AgentRow> {
    v.sort_by(|a, b| {
        a.activity
            .cmp(&b.activity)
            .then(b.last_activity.cmp(&a.last_activity))
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(path: &str, branch: Option<&str>) -> WorktreeRow {
        WorktreeRow {
            repo: "svc".into(),
            path: path.into(),
            name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into(),
            branch: branch.map(String::from),
            head: "deadbeef".into(),
            upstream: branch.map(|b| format!("origin/{b}")),
            ahead: 0,
            behind: 0,
            dirty: false,
            detached: branch.is_none(),
            is_main: path.ends_with("main"),
            missing: false,
        }
    }
    fn agent(pid: u32, cwd: &str, act: Activity) -> AgentRow {
        AgentRow {
            adapter: "opencode".into(),
            pid,
            cwd: cwd.into(),
            session_id: Some(format!("ses_{pid}")),
            title: Some("t".into()),
            mode: Some("plan".into()),
            activity: act,
            running_tool: None,
            plan_url: None,
            plan_title: None,
            port: None,
            last_activity: None,
            pane_target: None,
        }
    }

    #[test]
    fn joins_local_first() {
        let wts = vec![
            wt("/r/main", Some("main")),
            wt("/r/wt/x", Some("feat/x")),
            wt("/tmp/wt-head", None),
        ];
        let agents = vec![
            agent(10, "/r/wt/x", Activity::WaitPlan),
            agent(11, "/r/wt/x/services", Activity::Working),
            agent(12, "/home/me/.dotfiles", Activity::Idle),
        ];
        let panes = vec![PaneRow {
            session: "x".into(),
            window: 1,
            window_name: "w".into(),
            pane: 1,
            pid: 5,
            cwd: "/r/wt/x".into(),
            command: "opencode".into(),
            title: "".into(),
            attached: false,
        }];
        let reviews = vec![ReviewRow {
            source: "gitlab:a/b".into(),
            id: 7,
            display_id: "!7".into(),
            title: "x".into(),
            author: "me".into(),
            source_branch: "feat/x".into(),
            target_branch: "main".into(),
            draft: false,
            ci: Ci::Passed,
            approvals: vec![],
            threads: Threads::default(),
            status: ReviewStatus::Open,
            head_sha: "0000".into(),
            url: "u".into(),
            updated_at: None,
        }];
        let f = join(Inputs {
            panes,
            agents,
            worktrees: wts,
            reviews,
            procs: None,
            repo_sources: HashMap::new(),
        });

        assert_eq!(f.overview.len(), 4); // 3 worktrees + 1 outside
        let x = f.overview.iter().find(|r| r.name() == "x").unwrap();
        assert_eq!(x.agents.len(), 2);
        assert_eq!(x.overall, Overall::NeedsYou);
        assert_eq!(x.reviews[0].id, 7);
        assert_eq!(x.panes.len(), 1);
        assert_eq!(f.overview[0].name(), "x"); // NEEDS YOU sorts first
        let out = f.overview.last().unwrap();
        assert!(out.worktree.is_none());
        assert_eq!(out.agents[0].pid, 12);
        assert_eq!(f.reviews[0].worktrees.len(), 1);
        assert_eq!(f.reviews[0].agents.len(), 2);
        let dead = f
            .worktrees
            .iter()
            .find(|w| w.worktree.name == "wt-head")
            .unwrap();
        assert_eq!(dead.gc, GcVerdict::Yes("stale tmp".into()));
    }
}
