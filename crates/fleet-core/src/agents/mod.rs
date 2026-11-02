//! The agents collector family. One adapter per coding-agent CLI; opencode is the only one today.

pub mod opencode;

use crate::model::AgentRow;
use crate::procs::ProcTable;
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// What `launch` needs to build the command line that goes into the tmux pane.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub cwd: std::path::PathBuf,
    pub mode: String,
    /// `provider/model`; `None` = let the agent CLI pick (agent's own model, else global).
    pub model: Option<String>,
    pub prompt: String,
    pub port: u16,
    pub extra_args: Vec<String>,
}

/// One agent (mode) an agent CLI offers as the main agent.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentChoice {
    pub name: String,
    /// The agent's own default model, if its config sets one.
    pub model: Option<String>,
}

/// Everything a picker needs: the agents and models usable from one directory.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Choices {
    pub agents: Vec<AgentChoice>,
    /// `provider/model`, in the CLI's order.
    pub models: Vec<String>,
    /// The global default model.
    pub default_model: Option<String>,
}

impl Choices {
    /// What "default" means for this agent: its own model, else the global one.
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

    /// Error unless `model` is offered; names up to five close matches.
    pub fn check_model(&self, model: &str) -> Result<()> {
        if self.models.iter().any(|m| m == model) {
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

    /// Error unless `agent` is offered.
    pub fn check_agent(&self, agent: &str) -> Result<()> {
        if self.agents.is_empty() || self.agents.iter().any(|a| a.name == agent) {
            return Ok(());
        }
        bail!(
            "unknown agent `{agent}` (available: {})",
            self.agent_names().join(", ")
        )
    }
}

/// A message, optionally overriding the agent and/or model for that turn.
#[derive(Debug, Clone, Default)]
pub struct SendMsg {
    pub text: String,
    pub model: Option<String>,
    pub agent: Option<String>,
}

impl SendMsg {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
    pub fn has_override(&self) -> bool {
        self.model.is_some() || self.agent.is_some()
    }
}

/// Extra facts the engine knows that help an adapter identify its sessions.
#[derive(Debug, Default, Clone)]
pub struct DiscoverCtx {
    pub known_ports: HashMap<u32, u16>,
    pub title_hints: HashMap<u32, String>,
    /// old canonical path -> current canonical path (from `fleet agent rename`).
    pub path_aliases: HashMap<std::path::PathBuf, std::path::PathBuf>,
}

/// Where to deliver a message.
#[derive(Debug, Clone)]
pub enum SendTarget {
    /// The fleet plugin's control endpoint inside the agent process.
    Control { url: String, token: String },
    /// tmux pane target (`sess:w.p`) of an agent without the (current) plugin.
    Pane(String),
}

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    /// Every running agent of this adapter. `ctx.known_ports` = registry-known `pid -> port`
    /// for fleet-started agents (lets the adapter use the HTTP API); `ctx.title_hints` =
    /// `pid -> text` naming the session (the tmux pane title), used to pick the right
    /// session when several agents share one directory.
    async fn discover(&self, procs: &ProcTable, ctx: &DiscoverCtx) -> Result<Vec<AgentRow>>;

    /// Deliver a message to a running agent.
    async fn send(&self, target: &SendTarget, msg: &SendMsg) -> Result<()>;

    /// The shell command line that starts the agent in a tmux pane.
    fn launch(&self, spec: &LaunchSpec) -> String;

    /// Agents and models this CLI can use from `cwd` (credentials + that repo's config).
    async fn choices(&self, cwd: &Path) -> Result<Choices>;

    /// Is a process with this name one of ours?
    fn owns_process(&self, name: &str) -> bool;

    /// Used by `gc`: does the adapter consider `dir` in use (e.g. a live session)?
    fn uses_dir(&self, _dir: &Path) -> bool {
        false
    }
}

pub fn adapters(names: &[String]) -> Vec<Box<dyn AgentAdapter>> {
    names
        .iter()
        .filter_map(|n| match n.as_str() {
            "opencode" => Some(Box::new(opencode::OpenCode::default()) as Box<dyn AgentAdapter>),
            _ => None,
        })
        .collect()
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
    }
}
