//! The opencode adapter: procs + sqlite + (optional) HTTP + Plannotator probe -> `AgentRow`.

pub mod db;
pub mod http;
pub mod plannotator;

use super::{AgentAdapter, DiscoverCtx, LaunchSpec, SendTarget};
use crate::model::{Activity, AgentRow};
use crate::procs::{canon, ProcTable};
use crate::tmux;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct OpenCode {
    pub db_path: PathBuf,
    /// Sessions older than this are ignored when matching a process to a session.
    pub session_window_secs: i64,
}

impl Default for OpenCode {
    fn default() -> Self {
        Self {
            db_path: db::default_db_path(),
            session_window_secs: 14 * 86_400,
        }
    }
}

/// Pick the session for one process: the one whose title matches the hint (a tmux pane
/// title like `OC | Review MR 514…`, possibly truncated), else the newest in the directory.
pub fn pick_session<'a>(
    candidates: &[&'a db::DbSession],
    hint: Option<&str>,
) -> Option<&'a db::DbSession> {
    if let Some(h) = hint {
        let h = h
            .strip_prefix("OC | ")
            .unwrap_or(h)
            .trim_end_matches('…')
            .trim();
        if !h.is_empty() {
            if let Some(s) = candidates.iter().find(|s| {
                s.title.starts_with(h) || h.starts_with(s.title.as_str()) && !s.title.is_empty()
            }) {
                return Some(s);
            }
        }
    }
    candidates.first().copied()
}

/// Pure: turn the facts about one process into an `AgentRow`.
pub fn row_from_facts(
    pid: u32,
    cwd: PathBuf,
    session: Option<&db::DbSession>,
    plan: Option<&plannotator::Plan>,
    live: Option<&http::Live>,
    port: Option<u16>,
) -> AgentRow {
    let activity = if plan.is_some() {
        Activity::WaitPlan
    } else if live.map(|l| l.pending_permission).unwrap_or(false) {
        Activity::WaitPermission
    } else {
        match session {
            Some(s) if s.running_tool.as_deref() == Some("question") => Activity::WaitQuestion,
            Some(s) if s.running_tool.as_deref() == Some("submit_plan") => Activity::WaitPlan,
            Some(s) if !s.last_completed => Activity::Working,
            Some(_) => match live {
                Some(l) if l.busy => Activity::Working,
                _ => Activity::Idle,
            },
            None => match live {
                Some(l) if l.busy => Activity::Working,
                _ => Activity::Idle,
            },
        }
    };
    AgentRow {
        adapter: "opencode".into(),
        pid,
        cwd,
        session_id: session.map(|s| s.id.clone()),
        title: session.map(|s| s.title.clone()).filter(|t| !t.is_empty()),
        mode: session.and_then(|s| s.mode.clone()),
        activity,
        running_tool: session.and_then(|s| s.running_tool.clone()),
        plan_url: plan.map(|p| p.url.clone()),
        plan_title: plan.and_then(|p| p.title.clone()),
        port,
        last_activity: session.map(|s| s.updated),
    }
}

#[async_trait]
impl AgentAdapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn owns_process(&self, name: &str) -> bool {
        name == "opencode"
    }

    async fn discover(&self, procs: &ProcTable, ctx: &DiscoverCtx) -> Result<Vec<AgentRow>> {
        let known_ports = &ctx.known_ports;
        let pids: Vec<u32> = procs.named("opencode").iter().map(|p| p.pid).collect();
        if pids.is_empty() {
            return Ok(vec![]);
        }

        let db_path = self.db_path.clone();
        let window = self.session_window_secs;
        let sessions = tokio::task::spawn_blocking(move || db::recent_sessions(&db_path, window))
            .await
            .map_err(|e| anyhow::anyhow!("db task: {e}"))??;

        // Sessions per canonical directory, newest first.
        let mut by_dir: HashMap<PathBuf, Vec<&db::DbSession>> = HashMap::new();
        for s in &sessions {
            by_dir.entry(canon(&s.directory)).or_default().push(s);
        }

        let ports = plannotator::listening_ports(&pids).await;

        // Probe every listening port of every opencode process concurrently. A port is
        // either Plannotator (plan waiting), opencode's own API (fleet-started), or neither.
        let probes = pids.iter().flat_map(|&pid| {
            ports
                .get(&pid)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .map(move |&p| async move {
                    if let Some(pl) = plannotator::probe(p).await {
                        return (pid, p, Some(pl), false);
                    }
                    (pid, p, None, http::is_opencode(p).await)
                })
        });
        let probe_results = futures::future::join_all(probes).await;

        let mut rows = Vec::with_capacity(pids.len());
        for pid in pids {
            let cwd = procs
                .get(pid)
                .and_then(|p| p.cwd.clone())
                .map(|c| canon(&c))
                .unwrap_or_default();
            let session = pick_session(
                by_dir.get(&cwd).map(Vec::as_slice).unwrap_or(&[]),
                ctx.title_hints.get(&pid).map(String::as_str),
            );
            let mut plan = None;
            let mut port = known_ports.get(&pid).copied();
            for (p_pid, p, pl, is_oc) in &probe_results {
                if *p_pid != pid {
                    continue;
                }
                if let Some(pl) = pl {
                    plan = Some(pl.clone());
                } else if *is_oc && port.is_none() {
                    port = Some(*p);
                }
            }
            let live = match port {
                Some(p) => http::live(p).await,
                None => None,
            };
            rows.push(row_from_facts(
                pid,
                cwd,
                session,
                plan.as_ref(),
                live.as_ref(),
                port,
            ));
        }
        Ok(rows)
    }

    async fn send(&self, target: &SendTarget, text: &str) -> Result<()> {
        match target {
            SendTarget::Port(p) => http::submit_prompt(*p, text).await,
            SendTarget::Pane(t) => tmux::send_keys(t, text).await,
        }
    }

    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut parts = vec![
            "opencode".to_string(),
            "--port".into(),
            spec.port.to_string(),
            "--agent".into(),
            spec.mode.clone(),
        ];
        parts.extend(spec.extra_args.iter().cloned());
        parts.push("--prompt".into());
        parts.push(spec.prompt.clone());
        shell_words::join(parts)
    }

    fn uses_dir(&self, _dir: &Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sess(completed: bool, tool: Option<&str>) -> db::DbSession {
        db::DbSession {
            id: "ses_x".into(),
            title: "t".into(),
            directory: "/w".into(),
            mode: Some("build".into()),
            updated: Utc::now(),
            last_completed: completed,
            running_tool: tool.map(String::from),
        }
    }

    #[test]
    fn activity_rules() {
        let plan = plannotator::Plan {
            url: "http://127.0.0.1:1".into(),
            title: None,
        };
        assert_eq!(
            row_from_facts(
                1,
                "/w".into(),
                Some(&sess(false, Some("bash"))),
                Some(&plan),
                None,
                None
            )
            .activity,
            Activity::WaitPlan
        );
        assert_eq!(
            row_from_facts(
                1,
                "/w".into(),
                Some(&sess(false, Some("question"))),
                None,
                None,
                None
            )
            .activity,
            Activity::WaitQuestion
        );
        assert_eq!(
            row_from_facts(
                1,
                "/w".into(),
                Some(&sess(false, Some("submit_plan"))),
                None,
                None,
                None
            )
            .activity,
            Activity::WaitPlan
        );
        assert_eq!(
            row_from_facts(
                1,
                "/w".into(),
                Some(&sess(false, Some("bash"))),
                None,
                None,
                None
            )
            .activity,
            Activity::Working
        );
        assert_eq!(
            row_from_facts(1, "/w".into(), Some(&sess(true, None)), None, None, None).activity,
            Activity::Idle
        );
        let perm = http::Live {
            busy: true,
            pending_permission: true,
        };
        assert_eq!(
            row_from_facts(
                1,
                "/w".into(),
                Some(&sess(false, Some("bash"))),
                None,
                Some(&perm),
                Some(4096)
            )
            .activity,
            Activity::WaitPermission
        );
        assert_eq!(
            row_from_facts(1, "/w".into(), None, None, None, None).activity,
            Activity::Idle
        );
    }

    #[test]
    fn picks_session_by_pane_title() {
        let mut a = sess(true, None);
        a.id = "a".into();
        a.title = "Review GitLab MR 536 in monid-services".into();
        let mut b = sess(true, None);
        b.id = "b".into();
        b.title = "Multi-Agent Orchestration for Local Worktrees".into();
        let c = [&a, &b];
        assert_eq!(
            pick_session(&c, Some("OC | Multi-Agent Orchestration for Local W…"))
                .unwrap()
                .id,
            "b"
        );
        assert_eq!(
            pick_session(&c, Some("OC | Review GitLab MR 536 in monid-services"))
                .unwrap()
                .id,
            "a"
        );
        assert_eq!(pick_session(&c, Some("mac")).unwrap().id, "a");
        assert_eq!(pick_session(&c, None).unwrap().id, "a");
    }

    #[test]
    fn launch_line_is_shell_safe() {
        let oc = OpenCode::default();
        let s = oc.launch(&LaunchSpec {
            cwd: "/w".into(),
            mode: "plan".into(),
            prompt: "Fix the \"thing\"\n\nwith newlines".into(),
            port: 4096,
            extra_args: vec![],
        });
        assert!(s.starts_with("opencode --port 4096 --agent plan --prompt "));
        let back = shell_words::split(&s).unwrap();
        assert_eq!(back.last().unwrap(), "Fix the \"thing\"\n\nwith newlines");
    }
}
