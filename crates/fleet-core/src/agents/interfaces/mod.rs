//! What the rest of fleet depends on: the `AgentAdapter` trait, launch/send types, modes,
//! the agent-neutral state file and the hook event model. Implementations live in
//! `super::adapters`; nothing outside `agents` names a harness directly.

pub mod hooks;
pub mod state_file;

use super::adapters::{claude, codex, opencode};

use crate::config::AgentConfig;
use crate::model::AgentRow;
use crate::procs::ProcTable;
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// fleet's two named modes, which every adapter maps to its own flags, plus an
/// adapter-specific name on top (an opencode agent such as `reviewer`). Which names an
/// adapter accepts is what its [`AgentAdapter::choices`] lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Read-only: plan / ask before changing anything.
    Plan,
    /// May edit files and run commands (its own permission prompts still apply).
    Build,
    /// Another mode the adapter itself offers (opencode: any primary agent).
    Custom(String),
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() {
            "" => None,
            "plan" => Some(Mode::Plan),
            "build" => Some(Mode::Build),
            other => Some(Mode::Custom(other.to_string())),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Mode::Plan => "plan",
            Mode::Build => "build",
            Mode::Custom(s) => s,
        }
    }
}

/// What `launch` needs to build the command line that goes into the tmux pane.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub cwd: PathBuf,
    pub mode: Mode,
    /// Model in the adapter's own naming (opencode `provider/model`, claude `opus`, …).
    /// `None` = the adapter's configured `model`, else the CLI's own default.
    pub model: Option<String>,
    pub prompt: String,
    /// A free loopback port the adapter may use if it has an HTTP API (opencode `--port`).
    pub port: u16,
}

/// How long an adapter's discovered [`Choices`] stay fresh (new credentials or a new agent
/// show up after at most this long).
pub const CHOICES_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// One mode an adapter offers as the main agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentChoice {
    pub name: String,
    /// The mode's own default model, if the CLI's config sets one.
    pub model: Option<String>,
}

/// Everything a picker needs: the modes and models one adapter can use from one directory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Choices {
    pub agents: Vec<AgentChoice>,
    /// Models in the CLI's order. Empty = the CLI can't list them (any name is accepted).
    pub models: Vec<String>,
    /// The CLI's global default model.
    pub default_model: Option<String>,
}

impl Choices {
    /// fleet's two modes and no model list: what an adapter offers when its CLI can't say more.
    pub fn plan_build() -> Self {
        Self {
            agents: [Mode::Plan, Mode::Build]
                .iter()
                .map(|m| AgentChoice {
                    name: m.as_str().into(),
                    model: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// What "default" means for this mode: its own model, else the global one.
    pub fn default_for(&self, agent: &str) -> Option<&str> {
        self.agents
            .iter()
            .find(|a| a.name == agent)
            .and_then(|a| a.model.as_deref())
            .or(self.default_model.as_deref())
    }

    pub fn agent_names(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.name.clone()).collect()
    }

    /// Error unless `model` is offered; names up to five close matches. A CLI that lists
    /// no models accepts any name.
    pub fn check_model(&self, model: &str) -> Result<()> {
        if self.models.is_empty() || self.models.iter().any(|m| m == model) {
            return Ok(());
        }
        let needle = model.rsplit('/').next().unwrap_or(model).to_lowercase();
        let close: Vec<&str> = self
            .models
            .iter()
            .filter(|m| m.to_lowercase().contains(&needle))
            .take(5)
            .map(String::as_str)
            .collect();
        if close.is_empty() {
            bail!("unknown model `{model}` (`fleet agent choices` lists them)")
        }
        bail!(
            "unknown model `{model}`; did you mean: {}",
            close.join(", ")
        )
    }

    /// Error unless `agent` (a mode name) is offered.
    pub fn check_agent(&self, agent: &str) -> Result<()> {
        if self.agents.is_empty() || self.agents.iter().any(|a| a.name == agent) {
            return Ok(());
        }
        bail!(
            "unknown mode `{agent}` (available: {})",
            self.agent_names().join(", ")
        )
    }

    /// Check an optional mode and model; the shared rule for `start` and `send`.
    pub fn check(&self, mode: Option<&str>, model: Option<&str>) -> Result<()> {
        if let Some(m) = mode {
            self.check_agent(m)?;
        }
        if let Some(m) = model {
            self.check_model(m)?;
        }
        Ok(())
    }
}

/// A message, optionally overriding the mode and/or model for that turn.
#[derive(Debug, Clone, Default)]
pub struct SendMsg {
    pub text: String,
    pub model: Option<String>,
    /// Mode (opencode agent) for this turn.
    pub mode: Option<String>,
}

impl SendMsg {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
    pub fn has_override(&self) -> bool {
        self.model.is_some() || self.mode.is_some()
    }
}

/// Extra facts the engine knows that help an adapter identify its sessions.
#[derive(Debug, Default, Clone)]
pub struct DiscoverCtx {
    /// registry-known `pid -> port` for fleet-started agents.
    pub known_ports: HashMap<u32, u16>,
    /// `pid -> tmux pane title`, used to pick the right session when several agents share a dir.
    pub title_hints: HashMap<u32, String>,
    /// old canonical path -> current canonical path (from `fleet agent rename`).
    pub path_aliases: HashMap<PathBuf, PathBuf>,
    /// `pid -> state file` written by the plugin / hook, dead pids already pruned.
    pub states: HashMap<u32, state_file::AgentState>,
    /// `pid -> listening loopback ports` of the agent process and its descendants.
    pub ports: HashMap<u32, Vec<u16>>,
}

/// What `fleet setup` installs for an adapter.
#[derive(Debug, Clone)]
pub enum HookInstall {
    /// Write a whole file (opencode's plugin).
    File {
        path: PathBuf,
        contents: &'static str,
    },
    /// Merge command hooks into a JSON settings file (Claude Code, Codex).
    JsonHooks {
        path: PathBuf,
        events: &'static [&'static str],
        command: String,
    },
}

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// `opencode` | `claude` | `codex`
    fn name(&self) -> &'static str;

    /// Process names (as `ps` reports them) that belong to this adapter.
    fn process_names(&self) -> &'static [&'static str];

    /// Every running agent of this adapter.
    async fn discover(&self, procs: &ProcTable, ctx: &DiscoverCtx) -> Result<Vec<AgentRow>>;

    /// The shell command line that starts the agent in a tmux pane.
    fn launch(&self, spec: &LaunchSpec) -> String;

    /// Deliver a message to a running agent. The adapter decides the channel (the fleet
    /// plugin's control endpoint, `codex queue`, or typing into the tmux pane). A mode/model
    /// override that the adapter can't apply to one turn is an error, never silently dropped.
    async fn send(&self, agent: &AgentRow, msg: &SendMsg) -> Result<()>;

    /// Modes and models this CLI can use from `cwd` (credentials + that repo's config).
    /// Default: fleet's plan/build and no model list (any model name is passed through).
    async fn choices(&self, _cwd: &Path) -> Result<Choices> {
        Ok(Choices::plan_build())
    }

    /// What `fleet setup` installs so this adapter reports permission prompts.
    fn hook_install(&self) -> Option<HookInstall>;

    /// Does this process name belong to the adapter?
    fn owns_process(&self, name: &str) -> bool {
        self.process_names().contains(&name)
    }
}

/// Build the enabled adapters from config.
pub fn adapters(
    cfg: &crate::config::AgentsConfig,
    legacy_args: &[String],
) -> Vec<Box<dyn AgentAdapter>> {
    cfg.enabled()
        .into_iter()
        .filter_map(|n| {
            let mut c: AgentConfig = cfg.get(n);
            if n == "opencode" && c.args.is_empty() {
                c.args = legacy_args.to_vec();
            }
            adapter_for(n, c)
        })
        .collect()
}

pub fn adapter_for(name: &str, cfg: AgentConfig) -> Option<Box<dyn AgentAdapter>> {
    match name {
        "opencode" => Some(Box::new(opencode::OpenCode::new(cfg))),
        "claude" => Some(Box::new(claude::Claude::new(cfg))),
        "codex" => Some(Box::new(codex::Codex::new(cfg))),
        _ => None,
    }
}

pub const KNOWN: &[&str] = &["opencode", "claude", "codex"];

/// Shared helper: shell-quote a command line.
pub fn shell_line(parts: Vec<String>) -> String {
    shell_words::join(parts)
}

/// Shared helper: the tmux-pane fallback for `send`.
pub async fn send_via_tmux(agent: &AgentRow, text: &str) -> Result<()> {
    let target = agent
        .pane_target
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("agent has no tmux pane to type into"))?;
    crate::tmux::send_keys(target, text).await
}

/// Shared helper: error for a per-message mode/model switch on an adapter that has no way to
/// apply one to a single turn.
pub fn reject_override(adapter: &str, msg: &SendMsg) -> Result<()> {
    if msg.has_override() {
        bail!(
            "{adapter} can't switch mode or model for one message; send it without --mode/--model"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Choices {
        Choices {
            agents: vec![
                AgentChoice {
                    name: "build".into(),
                    model: None,
                },
                AgentChoice {
                    name: "plan".into(),
                    model: Some("openai/gpt-5.4".into()),
                },
            ],
            models: vec![
                "anthropic/claude-opus-5-5".into(),
                "anthropic/claude-sonnet-5".into(),
                "openai/gpt-5.4".into(),
            ],
            default_model: Some("anthropic/claude-opus-5-5".into()),
        }
    }

    #[test]
    fn default_model_per_agent() {
        let c = choices();
        assert_eq!(c.default_for("plan"), Some("openai/gpt-5.4"));
        assert_eq!(c.default_for("build"), Some("anthropic/claude-opus-5-5"));
        assert_eq!(c.default_for("nope"), Some("anthropic/claude-opus-5-5"));
    }

    #[test]
    fn checks() {
        let c = choices();
        assert!(c.check_model("openai/gpt-5.4").is_ok());
        let e = c.check_model("claude-opus-5-5").unwrap_err().to_string();
        assert!(e.contains("did you mean: anthropic/claude-opus-5-5"), "{e}");
        assert!(c.check_agent("plan").is_ok());
        assert!(c.check_agent("review").is_err());
        assert!(Choices::default().check_agent("anything").is_ok());
        // an adapter that lists no models (claude, codex) passes any name through
        let pb = Choices::plan_build();
        assert!(pb.check(Some("build"), Some("opus")).is_ok());
        assert!(pb.check(Some("reviewer"), None).is_err());
    }

    #[test]
    fn modes() {
        assert_eq!(Mode::parse("plan"), Some(Mode::Plan));
        assert_eq!(
            Mode::parse("reviewer"),
            Some(Mode::Custom("reviewer".into()))
        );
        assert_eq!(Mode::parse(" "), None);
        assert_eq!(Mode::Custom("reviewer".into()).as_str(), "reviewer");
        assert!(reject_override("claude", &SendMsg::text("hi")).is_ok());
        let m = SendMsg {
            text: "hi".into(),
            model: Some("opus".into()),
            mode: None,
        };
        assert!(reject_override("claude", &m).is_err());
    }
}
