//! The opencode adapter: procs + sqlite + (optional) HTTP + Plannotator probe -> `AgentRow`.

pub mod choices;
pub mod db;
pub mod http;
pub mod plannotator;

use crate::agents::interfaces::state_file::{self, AgentState};
use crate::agents::interfaces::{
    shell_line, AgentAdapter, Choices, DiscoverCtx, HookInstall, LaunchSpec, Mode, SendMsg,
};
use crate::config::AgentConfig;
use crate::model::{Activity, AgentRow};
use crate::procs::{canon, ProcTable};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct OpenCode {
    pub cfg: AgentConfig,
    pub db_path: PathBuf,
    /// Sessions older than this are ignored when matching a process to a session.
    pub session_window_secs: i64,
}

impl OpenCode {
    pub fn new(cfg: AgentConfig) -> Self {
        Self {
            cfg,
            db_path: db::default_db_path(),
            session_window_secs: 14 * 86_400,
        }
    }
    fn bin(&self) -> String {
        self.cfg.bin.clone().unwrap_or_else(|| "opencode".into())
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
    state: Option<&AgentState>,
) -> AgentRow {
    let pending = state.map(|s| s.pending()).unwrap_or_default();
    let control = state.and_then(|s| s.control());
    let activity = if plan.is_some() {
        Activity::WaitPlan
    } else if !pending.is_empty() || live.map(|l| l.pending_permission).unwrap_or(false) {
        Activity::WaitPermission
    } else {
        match session {
            Some(s) if s.running_tool.as_deref() == Some("question") => Activity::WaitQuestion,
            Some(s) if s.running_tool.as_deref() == Some("submit_plan") => Activity::WaitPlan,
            Some(s) if !s.last_completed => Activity::Working,
            _ => match live {
                Some(l) if l.busy => Activity::Working,
                _ => Activity::Idle,
            },
        }
    };
    AgentRow {
        machine: None,
        adapter: "opencode".into(),
        pid,
        cwd,
        session_id: session.map(|s| s.id.clone()),
        title: session.map(|s| s.title.clone()).filter(|t| !t.is_empty()),
        mode: session.and_then(|s| s.mode.clone()),
        model: session.and_then(|s| s.model.clone()),
        activity,
        running_tool: session.and_then(|s| s.running_tool.clone()),
        plan_url: plan.map(|p| p.url.clone()),
        plan_title: plan.and_then(|p| p.title.clone()),
        port,
        last_activity: session.map(|s| s.updated),
        pane_target: None,
        pending: pending
            .iter()
            .map(|p| {
                if p.title.is_empty() {
                    p.kind.clone()
                } else {
                    p.title.clone()
                }
            })
            .collect(),
        control_url: control.as_ref().map(|c| c.0.clone()),
        control_token: control.map(|c| c.1),
    }
}

#[async_trait]
impl AgentAdapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn process_names(&self) -> &'static [&'static str] {
        &["opencode"]
    }

    async fn discover(&self, procs: &ProcTable, ctx: &DiscoverCtx) -> Result<Vec<AgentRow>> {
        let pids: Vec<u32> = procs.named("opencode").iter().map(|p| p.pid).collect();
        if pids.is_empty() {
            return Ok(vec![]);
        }

        let db_path = self.db_path.clone();
        let window = self.session_window_secs;
        let sessions = tokio::task::spawn_blocking(move || db::recent_sessions(&db_path, window))
            .await
            .map_err(|e| anyhow::anyhow!("db task: {e}"))??;

        // Sessions per canonical directory, newest first. A session started in a path that
        // was later renamed (`fleet agent rename`) is indexed under the new path too.
        let mut by_dir: HashMap<PathBuf, Vec<&db::DbSession>> = HashMap::new();
        for s in &sessions {
            let dir = canon(&s.directory);
            let dir = ctx.path_aliases.get(&dir).cloned().unwrap_or(dir);
            by_dir.entry(dir).or_default().push(s);
        }
        let by_id: HashMap<&str, &db::DbSession> =
            sessions.iter().map(|s| (s.id.as_str(), s)).collect();

        // Probe every listening port of every opencode process concurrently. A port is
        // either Plannotator (plan waiting), opencode's own API, or neither.
        let probes = pids.iter().flat_map(|&pid| {
            ctx.ports
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
            let state = ctx
                .states
                .get(&pid)
                .filter(|s| s.agent.is_empty() || s.agent == "opencode");
            let state_port = state
                .and_then(|st| st.port())
                .filter(|p| ctx.ports.get(&pid).map(|v| v.contains(p)).unwrap_or(false));
            let session = state
                .and_then(|st| st.primary_session())
                .and_then(|(id, _)| by_id.get(id.as_str()).copied())
                .or_else(|| {
                    pick_session(
                        by_dir.get(&cwd).map(Vec::as_slice).unwrap_or(&[]),
                        ctx.title_hints.get(&pid).map(String::as_str),
                    )
                });
            let mut plan = None;
            let mut port = ctx.known_ports.get(&pid).copied().or(state_port);
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
                state,
            ));
        }
        Ok(rows)
    }

    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut parts = vec![
            self.bin(),
            "--port".into(),
            spec.port.to_string(),
            "--agent".into(),
            // opencode has no permission layer of its own: yolo is build.
            match spec.mode {
                Mode::Yolo => "build".to_string(),
                ref m => m.as_str().to_string(),
            },
        ];
        if let Some(m) = spec.model.as_ref().or(self.cfg.model.as_ref()) {
            parts.push("--model".into());
            parts.push(m.clone());
        }
        parts.extend(self.cfg.args.iter().cloned());
        parts.push("--prompt".into());
        parts.push(spec.combined());
        shell_line(parts)
    }

    fn skill_dirs(&self) -> Vec<PathBuf> {
        vec![
            crate::config::xdg_config_home()
                .join("opencode")
                .join("skills"),
            crate::config::home().join(".agents").join("skills"),
        ]
    }

    /// Through the fleet plugin's control endpoint when the process has one (plain and
    /// override messages alike land in its current session); else tmux keystrokes, which
    /// can't switch mode or model.
    async fn send(&self, agent: &AgentRow, msg: &SendMsg) -> Result<()> {
        if let (Some(url), Some(token)) = (&agent.control_url, &agent.control_token) {
            return http::control_send(url, token, msg).await;
        }
        if msg.has_override() {
            anyhow::bail!(
                "this agent has no fleet control endpoint, so its mode/model can't be switched: \
                 run `fleet setup` and restart the agent"
            );
        }
        crate::agents::interfaces::send_via_tmux(agent, &msg.text).await
    }

    async fn choices(&self, cwd: &Path) -> Result<Choices> {
        choices::choices(&self.bin(), cwd).await
    }

    fn hook_install(&self) -> Option<HookInstall> {
        Some(HookInstall::File {
            path: plugin_path(),
            contents: state_file::PLUGIN_SOURCE,
        })
    }
}

pub fn plugin_path() -> PathBuf {
    crate::config::xdg_config_home()
        .join("opencode")
        .join("plugins")
        .join("fleet.ts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Mode;
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
            model: Some("anthropic/claude-opus-5-5".into()),
        }
    }

    #[test]
    fn activity_rules() {
        let plan = plannotator::Plan {
            url: "http://127.0.0.1:1".into(),
            title: None,
        };
        let f = |s: Option<&db::DbSession>,
                 pl: Option<&plannotator::Plan>,
                 live: Option<&http::Live>,
                 st: Option<&AgentState>| {
            row_from_facts(1, "/w".into(), s, pl, live, None, st).activity
        };
        assert_eq!(
            f(Some(&sess(false, Some("bash"))), Some(&plan), None, None),
            Activity::WaitPlan
        );
        assert_eq!(
            f(Some(&sess(false, Some("question"))), None, None, None),
            Activity::WaitQuestion
        );
        assert_eq!(
            f(Some(&sess(false, Some("submit_plan"))), None, None, None),
            Activity::WaitPlan
        );
        assert_eq!(
            f(Some(&sess(false, Some("bash"))), None, None, None),
            Activity::Working
        );
        assert_eq!(f(Some(&sess(true, None)), None, None, None), Activity::Idle);
        assert_eq!(f(None, None, None, None), Activity::Idle);
        let st = state_file::parse(r#"{"pid":1,"sessions":{"ses_x":{"status":"busy","pending":[{"id":"p","title":"bash: rm -rf dist","type":"bash"}]}}}"#).unwrap();
        let row = row_from_facts(
            1,
            "/w".into(),
            Some(&sess(false, Some("bash"))),
            None,
            None,
            None,
            Some(&st),
        );
        assert_eq!(row.activity, Activity::WaitPermission);
        assert_eq!(row.pending, vec!["bash: rm -rf dist".to_string()]);
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
        let oc = OpenCode::new(AgentConfig::default());
        let mut spec = LaunchSpec {
            cwd: "/w".into(),
            mode: Mode::Plan,
            model: None,
            system: String::new(),
            prompt: "Fix the \"thing\"\n\nwith newlines".into(),
            port: 4096,
        };
        let s = oc.launch(&spec);
        assert!(s.starts_with("opencode --port 4096 --agent plan --prompt "));
        let back = shell_words::split(&s).unwrap();
        assert_eq!(back.last().unwrap(), "Fix the \"thing\"\n\nwith newlines");
        // configured model, overridden per launch; custom agents pass through
        let oc = OpenCode::new(AgentConfig {
            model: Some("anthropic/claude-opus-5-5".into()),
            ..Default::default()
        });
        spec.mode = Mode::Custom("reviewer".into());
        assert!(oc.launch(&spec).starts_with(
            "opencode --port 4096 --agent reviewer --model anthropic/claude-opus-5-5 --prompt "
        ));
        spec.model = Some("openai/gpt-5.4".into());
        assert!(oc
            .launch(&spec)
            .starts_with("opencode --port 4096 --agent reviewer --model openai/gpt-5.4 --prompt "));
    }

    #[test]
    fn control_endpoint_from_state_file() {
        let st = state_file::parse(r#"{"pid":1,"agent":"opencode","control_url":"http://127.0.0.1:9","token":"t","current_session":"ses_x","sessions":{"ses_x":{"status":"idle"}}}"#).unwrap();
        let row = row_from_facts(
            1,
            "/w".into(),
            Some(&sess(true, None)),
            None,
            None,
            None,
            Some(&st),
        );
        assert_eq!(row.control_url.as_deref(), Some("http://127.0.0.1:9"));
        assert_eq!(row.control_token.as_deref(), Some("t"));
        assert_eq!(row.model.as_deref(), Some("anthropic/claude-opus-5-5"));
    }
}
