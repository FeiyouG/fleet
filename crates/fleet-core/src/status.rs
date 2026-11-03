//! Pure status rules.

use crate::model::{
    Activity, AgentRow, GcVerdict, Overall, ReviewRow, ReviewStatus, Sync, WorktreeRow,
};
use std::path::Path;

/// The Overview verdict for one worktree (or outside-repo dir), first match wins.
pub fn overall(
    worktree: Option<&WorktreeRow>,
    agents: &[AgentRow],
    review: Option<&ReviewRow>,
) -> Overall {
    if agents.iter().any(|a| a.activity.is_waiting()) {
        return Overall::NeedsYou;
    }
    if let Some(r) = review {
        if matches!(r.status, ReviewStatus::CiFailed | ReviewStatus::Conflicts) {
            return Overall::Failed;
        }
    }
    let all_idle = agents
        .iter()
        .all(|a| matches!(a.activity, Activity::Idle | Activity::None));
    if let (Some(w), Some(r)) = (worktree, review) {
        if all_idle
            && !r.status.is_done()
            && w.sync() == Sync::Synced
            && !w.dirty
            && r.ci == crate::model::Ci::Passed
        {
            return Overall::Ready;
        }
        if r.status == ReviewStatus::Merged {
            return Overall::Merged;
        }
    }
    if agents.is_empty() {
        return Overall::Dead;
    }
    if all_idle {
        Overall::Idle
    } else {
        Overall::Working
    }
}

fn is_tmp(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("/private/tmp/")
        || s.starts_with("/tmp/")
        || s.starts_with("/private/var/folders/")
        || s.starts_with("/var/folders/")
}

/// Would `gc` remove this worktree, and why / why not? Only finished work qualifies; the
/// close planner then checks it again (uncommitted files, unmerged commits) before anything
/// is removed, so a "yes" here can still end up "blocked".
pub fn gc_verdict(w: &WorktreeRow, agents: &[AgentRow], review: Option<&ReviewRow>) -> GcVerdict {
    if w.is_main {
        return GcVerdict::No("main clone".into());
    }
    if agents.iter().any(|a| a.activity.is_busy()) {
        return GcVerdict::No("agent busy".into());
    }
    if w.missing {
        return GcVerdict::Yes("folder missing".into());
    }
    if w.dirty {
        return GcVerdict::No("dirty".into());
    }
    // An idle agent on merged work is stopped by gc; on anything else it means "in use".
    if let Some(r) = review {
        if r.status.is_done() {
            return GcVerdict::Yes(r.status.label().into());
        }
    }
    if !agents.is_empty() {
        return GcVerdict::No("agent alive".into());
    }
    if w.in_base && !w.detached && review.is_none() {
        return GcVerdict::Yes("in base".into());
    }
    if w.detached && is_tmp(&w.path) {
        return GcVerdict::Yes("stale tmp".into());
    }
    if w.detached && w.name.starts_with("review-") && review.is_none() {
        return GcVerdict::Yes("review gone".into());
    }
    GcVerdict::No("in use".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ci, Threads};

    fn wt(ahead: u32, behind: u32, dirty: bool) -> WorktreeRow {
        WorktreeRow {
            repo: "r".into(),
            path: "/w/x".into(),
            name: "x".into(),
            branch: Some("feat/x".into()),
            head: "abc".into(),
            upstream: Some("origin/feat/x".into()),
            ahead,
            behind,
            dirty,
            detached: false,
            is_main: false,
            missing: false,
            in_base: false,
        }
    }
    fn agent(a: Activity) -> AgentRow {
        AgentRow {
            adapter: "opencode".into(),
            pid: 1,
            cwd: "/w/x".into(),
            session_id: None,
            title: None,
            mode: None,
            activity: a,
            running_tool: None,
            plan_url: None,
            plan_title: None,
            port: None,
            last_activity: None,
            pane_target: None,
            pending: vec![],
        }
    }
    fn review(status: ReviewStatus, ci: Ci) -> ReviewRow {
        ReviewRow {
            source: "gitlab:a/b".into(),
            id: 1,
            display_id: "!1".into(),
            title: "t".into(),
            author: "me".into(),
            source_branch: "feat/x".into(),
            target_branch: "main".into(),
            draft: false,
            ci,
            approvals: vec![],
            threads: Threads::default(),
            status,
            head_sha: "s".into(),
            url: "u".into(),
            updated_at: None,
        }
    }

    #[test]
    fn overall_rules() {
        let w = wt(0, 0, false);
        assert_eq!(
            overall(Some(&w), &[agent(Activity::WaitPlan)], None),
            Overall::NeedsYou
        );
        assert_eq!(
            overall(
                Some(&w),
                &[agent(Activity::Working)],
                Some(&review(ReviewStatus::CiFailed, Ci::Failed))
            ),
            Overall::Failed
        );
        assert_eq!(
            overall(
                Some(&w),
                &[agent(Activity::Idle)],
                Some(&review(ReviewStatus::Approved, Ci::Passed))
            ),
            Overall::Ready
        );
        assert_eq!(
            overall(
                Some(&wt(2, 0, false)),
                &[agent(Activity::Idle)],
                Some(&review(ReviewStatus::Open, Ci::Passed))
            ),
            Overall::Idle
        );
        assert_eq!(
            overall(
                Some(&w),
                &[],
                Some(&review(ReviewStatus::Merged, Ci::Passed))
            ),
            Overall::Merged
        );
        assert_eq!(overall(Some(&w), &[], None), Overall::Dead);
        assert_eq!(
            overall(Some(&w), &[agent(Activity::Working)], None),
            Overall::Working
        );
    }

    #[test]
    fn gc_rules() {
        // A busy agent keeps it; an idle one keeps it unless the work is merged.
        assert_eq!(
            gc_verdict(&wt(0, 0, false), &[agent(Activity::Working)], None),
            GcVerdict::No("agent busy".into())
        );
        assert_eq!(
            gc_verdict(&wt(0, 0, false), &[agent(Activity::Idle)], None),
            GcVerdict::No("agent alive".into())
        );
        assert_eq!(
            gc_verdict(
                &wt(0, 0, false),
                &[agent(Activity::Idle)],
                Some(&review(ReviewStatus::Merged, Ci::None))
            ),
            GcVerdict::Yes("merged".into())
        );
        assert_eq!(
            gc_verdict(
                &wt(0, 0, false),
                &[agent(Activity::WaitQuestion)],
                Some(&review(ReviewStatus::Merged, Ci::None))
            ),
            GcVerdict::No("agent busy".into())
        );
        // Pushed or not is not the question any more: the planner checks "merged".
        assert_eq!(
            gc_verdict(&wt(2, 0, false), &[], None),
            GcVerdict::No("in use".into())
        );
        assert_eq!(
            gc_verdict(&wt(0, 0, true), &[], None),
            GcVerdict::No("dirty".into())
        );
        let mut b = wt(0, 0, false);
        b.in_base = true;
        assert_eq!(gc_verdict(&b, &[], None), GcVerdict::Yes("in base".into()));
        assert_eq!(
            gc_verdict(&b, &[agent(Activity::Idle)], None),
            GcVerdict::No("agent alive".into())
        );
        let mut t = wt(0, 0, false);
        t.detached = true;
        t.upstream = None;
        t.path = "/private/tmp/wt-head".into();
        assert_eq!(
            gc_verdict(&t, &[], None),
            GcVerdict::Yes("stale tmp".into())
        );
    }
}
