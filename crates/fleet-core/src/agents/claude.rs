//! Claude Code adapter. Live state via `fleet agent-hook claude` in
//! `~/.claude/settings.json`; fallback identity from `~/.claude/projects/<slug>/*.jsonl`.

use super::hooked::{self, Fallback};
use super::{shell_line, AgentAdapter, DiscoverCtx, HookInstall, LaunchSpec, Mode};
use crate::config::{home, AgentConfig};
use crate::model::AgentRow;
use crate::procs::{canon, ProcTable};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

pub struct Claude {
    pub cfg: AgentConfig,
}

impl Claude {
    pub fn new(cfg: AgentConfig) -> Self {
        Self { cfg }
    }
    fn bin(&self) -> String {
        self.cfg.bin.clone().unwrap_or_else(|| "claude".into())
    }
}

pub fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
}

pub fn settings_path() -> PathBuf {
    claude_dir().join("settings.json")
}

/// `~/.claude/projects/<slug>` where slug = cwd with every `/` and `.` turned into `-`.
pub fn project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Newest transcript for a cwd: session id = file stem, title = first user text.
pub fn fallback_for(cwd: &Path) -> Fallback {
    let dir = claude_dir().join("projects").join(project_slug(cwd));
    let Some((path, mtime)) = hooked::newest_file(&dir, |p| {
        p.extension().and_then(|e| e.to_str()) == Some("jsonl")
    }) else {
        return Fallback::default();
    };
    let session_id = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let title = std::fs::read_to_string(&path).ok().and_then(|text| {
        text.lines().take(200).find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v.get("type").and_then(|t| t.as_str()) != Some("user") {
                return None;
            }
            let content = v.pointer("/message/content")?;
            let s = content.as_str().map(String::from).or_else(|| {
                content
                    .as_array()?
                    .iter()
                    .find_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
            })?;
            let first = s.lines().find(|l| !l.trim().is_empty())?.trim();
            if first.starts_with('<') {
                return None; // system-reminder / tool blocks
            }
            Some(first.chars().take(80).collect())
        })
    });
    Fallback {
        session_id,
        title,
        last_activity: Some(mtime),
    }
}

#[async_trait]
impl AgentAdapter for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn process_names(&self) -> &'static [&'static str] {
        &["claude"]
    }

    async fn discover(&self, procs: &ProcTable, ctx: &DiscoverCtx) -> Result<Vec<AgentRow>> {
        let mut rows = vec![];
        for p in procs.named("claude") {
            let cwd = p.cwd.clone().map(|c| canon(&c)).unwrap_or_default();
            let state = ctx.states.get(&p.pid).filter(|s| s.agent == "claude");
            let plan = hooked::plan_for(p.pid, ctx).await;
            let fb = if state.is_some() {
                Fallback::default()
            } else {
                fallback_for(&cwd)
            };
            rows.push(hooked::row("claude", p.pid, cwd, state, plan.as_ref(), fb));
        }
        Ok(rows)
    }

    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut parts = vec![self.bin()];
        if spec.mode == Mode::Plan {
            parts.extend(["--permission-mode".to_string(), "plan".to_string()]);
        }
        parts.extend(self.cfg.args.iter().cloned());
        parts.push("--".into());
        parts.push(spec.prompt.clone());
        shell_line(parts)
    }

    async fn send(&self, agent: &AgentRow, text: &str) -> Result<()> {
        super::send_via_tmux(agent, text).await
    }

    fn hook_install(&self) -> Option<HookInstall> {
        Some(HookInstall::JsonHooks {
            path: settings_path(),
            events: super::hooks::EVENTS,
            command: "fleet agent-hook claude".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_launch() {
        assert_eq!(
            project_slug(Path::new("/Users/me/Projects/monid.services")),
            "-Users-me-Projects-monid-services"
        );
        let c = Claude::new(AgentConfig {
            args: vec!["--model".into(), "opus".into()],
            ..Default::default()
        });
        let s = c.launch(&LaunchSpec {
            cwd: "/w".into(),
            mode: Mode::Plan,
            prompt: "do \"it\"".into(),
            port: 0,
        });
        assert_eq!(
            s,
            "claude --permission-mode plan --model opus -- 'do \"it\"'"
        );
        let s = c.launch(&LaunchSpec {
            cwd: "/w".into(),
            mode: Mode::Build,
            prompt: "x".into(),
            port: 0,
        });
        assert_eq!(s, "claude --model opus -- x");
    }
}
