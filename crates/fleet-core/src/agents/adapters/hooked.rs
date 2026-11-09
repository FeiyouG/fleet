//! Shared logic for adapters whose live state comes from `fleet agent-hook` state files
//! (Claude Code, Codex): the state file is the primary source; without one, the agent is
//! visible but only as `IDLE` with whatever its own session storage reveals.

use super::opencode::plannotator;
use crate::agents::interfaces::state_file::AgentState;
use crate::agents::interfaces::DiscoverCtx;
use crate::model::{Activity, AgentRow};
use chrono::{DateTime, Utc};
use std::path::PathBuf;

/// Facts an adapter can recover from the agent's own storage when no state file exists.
#[derive(Debug, Clone, Default)]
pub struct Fallback {
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub last_activity: Option<DateTime<Utc>>,
}

pub fn row(
    adapter: &'static str,
    pid: u32,
    cwd: PathBuf,
    state: Option<&AgentState>,
    plan: Option<&plannotator::Plan>,
    fb: Fallback,
) -> AgentRow {
    let sess = state.and_then(|s| s.primary_session());
    let pending: Vec<String> = state
        .map(|s| {
            s.pending()
                .iter()
                .map(|p| {
                    if p.title.is_empty() {
                        p.kind.clone()
                    } else {
                        p.title.clone()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let activity = if plan.is_some() || sess.map(|(_, s)| s.plan).unwrap_or(false) {
        Activity::WaitPlan
    } else if !pending.is_empty() {
        Activity::WaitPermission
    } else if sess.map(|(_, s)| s.question).unwrap_or(false) {
        Activity::WaitQuestion
    } else if sess.map(|(_, s)| s.status == "busy").unwrap_or(false) {
        Activity::Working
    } else {
        Activity::Idle
    };
    let mode = sess
        .and_then(|(_, s)| s.mode.clone())
        .map(|m| match m.as_str() {
            "plan" => "plan".to_string(),
            "default" | "acceptEdits" | "auto" | "dontAsk" | "bypassPermissions" => {
                "build".to_string()
            }
            other => other.to_string(),
        });
    AgentRow {
        adapter: adapter.into(),
        pid,
        cwd,
        session_id: sess.map(|(id, _)| id.clone()).or(fb.session_id),
        title: sess.and_then(|(_, s)| s.title.clone()).or(fb.title),
        mode,
        model: None,
        activity,
        running_tool: None,
        plan_url: plan.map(|p| p.url.clone()),
        plan_title: plan.and_then(|p| p.title.clone()),
        port: None,
        last_activity: state
            .map(|s| Utc.timestamp_millis_opt(s.updated_at).single())
            .flatten()
            .or(fb.last_activity),
        pane_target: None,
        pending,
        control_url: None,
        control_token: None,
    }
}

/// Probe this pid's ports (which include its descendants') for a Plannotator server.
pub async fn plan_for(pid: u32, ctx: &DiscoverCtx) -> Option<plannotator::Plan> {
    for &p in ctx.ports.get(&pid).map(Vec::as_slice).unwrap_or(&[]) {
        if let Some(pl) = plannotator::probe(p).await {
            return Some(pl);
        }
    }
    None
}

use chrono::TimeZone;

/// Newest file under `dir` (non-recursive) matching `pred`, with its mtime.
pub fn newest_file(
    dir: &std::path::Path,
    pred: impl Fn(&std::path::Path) -> bool,
) -> Option<(PathBuf, DateTime<Utc>)> {
    let rd = std::fs::read_dir(dir).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| pred(p))
        .filter_map(|p| {
            let m = std::fs::metadata(&p).ok()?.modified().ok()?;
            Some((p, DateTime::<Utc>::from(m)))
        })
        .max_by_key(|(_, t)| *t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::state_file::{self};

    #[test]
    fn activity_from_state() {
        let st = state_file::parse(r#"{"pid":1,"agent":"claude","sessions":{"s":{"status":"busy","pending":[],"question":true,"mode":"plan","title":"Fix it"}}}"#).unwrap();
        let r = row(
            "claude",
            1,
            "/w".into(),
            Some(&st),
            None,
            Fallback::default(),
        );
        assert_eq!(r.activity, Activity::WaitQuestion);
        assert_eq!(r.mode.as_deref(), Some("plan"));
        assert_eq!(r.title.as_deref(), Some("Fix it"));
        let st = state_file::parse(r#"{"pid":1,"sessions":{"s":{"status":"busy","pending":[{"title":"Bash: rm -rf x"}],"mode":"default"}}}"#).unwrap();
        let r = row(
            "codex",
            1,
            "/w".into(),
            Some(&st),
            None,
            Fallback::default(),
        );
        assert_eq!(r.activity, Activity::WaitPermission);
        assert_eq!(r.mode.as_deref(), Some("build"));
        assert_eq!(r.pending, vec!["Bash: rm -rf x"]);
        let r = row(
            "claude",
            1,
            "/w".into(),
            None,
            None,
            Fallback {
                title: Some("old".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            (r.activity, r.title.as_deref()),
            (Activity::Idle, Some("old"))
        );
    }
}
