//! Codex CLI adapter. Live state via `fleet agent-hook codex` in `~/.codex/hooks.json`;
//! fallback identity from `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (`session_meta.cwd`)
//! and `~/.codex/session_index.jsonl` (thread names). Messages via `codex queue`.

use super::hooked::{self, Fallback};
use super::{shell_line, AgentAdapter, DiscoverCtx, HookInstall, LaunchSpec, Mode};
use crate::config::{home, AgentConfig};
use crate::model::AgentRow;
use crate::procs::{canon, ProcTable};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub struct Codex {
    pub cfg: AgentConfig,
}

impl Codex {
    pub fn new(cfg: AgentConfig) -> Self {
        Self { cfg }
    }
    fn bin(&self) -> String {
        self.cfg.bin.clone().unwrap_or_else(|| "codex".into())
    }
}

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
}

pub fn hooks_path() -> PathBuf {
    codex_home().join("hooks.json")
}

/// Newest rollout whose `session_meta.cwd` is `cwd`, scanning today's and yesterday's folders.
pub fn fallback_for(cwd: &Path) -> Fallback {
    let sessions = codex_home().join("sessions");
    let now = Utc::now();
    let mut best: Option<(String, DateTime<Utc>)> = None;
    for days_ago in 0..3 {
        let d = now - chrono::Duration::days(days_ago);
        let dir = sessions
            .join(d.format("%Y").to_string())
            .join(d.format("%m").to_string())
            .join(d.format("%d").to_string());
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if !path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with("rollout-"))
                .unwrap_or(false)
            {
                continue;
            }
            let Some(meta) = first_line_meta(&path) else {
                continue;
            };
            if meta.1.as_deref() != Some(&cwd.to_string_lossy()) {
                continue;
            }
            let mtime = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(DateTime::<Utc>::from)
                .unwrap_or(now);
            if best.as_ref().map(|(_, t)| mtime > *t).unwrap_or(true) {
                best = Some((meta.0, mtime));
            }
        }
    }
    let Some((id, mtime)) = best else {
        return Fallback::default();
    };
    let title = thread_name(&id);
    Fallback {
        session_id: Some(id),
        title,
        last_activity: Some(mtime),
    }
}

/// `(thread id, cwd)` from a rollout file's first line.
fn first_line_meta(path: &Path) -> Option<(String, Option<String>)> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let first = BufReader::new(f).lines().next()?.ok()?;
    let v: serde_json::Value = serde_json::from_str(&first).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return None;
    }
    let p = v.get("payload")?;
    Some((
        p.get("id")?.as_str()?.to_string(),
        p.get("cwd").and_then(|c| c.as_str()).map(String::from),
    ))
}

/// Last `thread_name` recorded for a thread id in `session_index.jsonl`.
fn thread_name(id: &str) -> Option<String> {
    let text = std::fs::read_to_string(codex_home().join("session_index.jsonl")).ok()?;
    text.lines().rev().find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        (v.get("id")?.as_str()? == id).then(|| v.get("thread_name")?.as_str().map(String::from))?
    })
}

#[async_trait]
impl AgentAdapter for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn process_names(&self) -> &'static [&'static str] {
        &["codex"]
    }

    async fn discover(&self, procs: &ProcTable, ctx: &DiscoverCtx) -> Result<Vec<AgentRow>> {
        let mut rows = vec![];
        for p in procs.named("codex") {
            // The ChatGPT desktop app bundles `codex app-server`; that is not a TUI session.
            if p.cmdline.iter().any(|a| a == "app-server") {
                continue;
            }
            let cwd = p.cwd.clone().map(|c| canon(&c)).unwrap_or_default();
            let state = ctx.states.get(&p.pid).filter(|s| s.agent == "codex");
            let plan = hooked::plan_for(p.pid, ctx).await;
            let fb = if state.is_some() {
                Fallback::default()
            } else {
                fallback_for(&cwd)
            };
            rows.push(hooked::row("codex", p.pid, cwd, state, plan.as_ref(), fb));
        }
        Ok(rows)
    }

    fn launch(&self, spec: &LaunchSpec) -> String {
        let sandbox = match spec.mode {
            Mode::Plan => "read-only".to_string(),
            Mode::Build => self
                .cfg
                .sandbox
                .clone()
                .unwrap_or_else(|| "workspace-write".into()),
        };
        let mut parts = vec![
            self.bin(),
            "--cd".into(),
            spec.cwd.to_string_lossy().into_owned(),
            "--ask-for-approval".into(),
            "on-request".into(),
            "--sandbox".into(),
            sandbox,
        ];
        parts.extend(self.cfg.args.iter().cloned());
        parts.push(spec.prompt.clone());
        shell_line(parts)
    }

    async fn send(&self, agent: &AgentRow, text: &str) -> Result<()> {
        if let Some(id) = &agent.session_id {
            let out = Command::new(self.bin())
                .args(["queue", "--thread", id, "--message", text])
                .output()
                .await
                .context("running codex queue")?;
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr);
            if agent.pane_target.is_none() {
                bail!("codex queue failed: {}", err.trim());
            }
        }
        super::send_via_tmux(agent, text).await
    }

    fn hook_install(&self) -> Option<HookInstall> {
        Some(HookInstall::JsonHooks {
            path: hooks_path(),
            events: super::hooks::EVENTS,
            command: "fleet agent-hook codex".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_modes() {
        let c = Codex::new(AgentConfig::default());
        let s = c.launch(&LaunchSpec {
            cwd: "/w t".into(),
            mode: Mode::Plan,
            prompt: "hi".into(),
            port: 0,
        });
        assert_eq!(
            s,
            "codex --cd '/w t' --ask-for-approval on-request --sandbox read-only hi"
        );
        let c = Codex::new(AgentConfig {
            sandbox: Some("danger-full-access".into()),
            ..Default::default()
        });
        assert!(c
            .launch(&LaunchSpec {
                cwd: "/w".into(),
                mode: Mode::Build,
                prompt: "x".into(),
                port: 0
            })
            .contains("--sandbox danger-full-access"));
    }

    #[test]
    fn parses_rollout_meta() {
        let dir = std::env::temp_dir().join(format!("fleet-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rollout-x.jsonl");
        std::fs::write(
            &p,
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"01a1","cwd":"/w"}}
{"type":"other"}"#,
        )
        .unwrap();
        assert_eq!(
            first_line_meta(&p),
            Some(("01a1".into(), Some("/w".into())))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
