//! The agents collector family. One adapter per coding-agent CLI. Adapters are independent of
//! projects: any enabled agent can be launched in any project.

pub mod claude;
pub mod codex;
pub mod hooked;
pub mod hooks;
pub mod opencode;
pub mod state_file;

use crate::config::AgentConfig;
use crate::model::AgentRow;
use crate::procs::ProcTable;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;

/// fleet's two named modes; each adapter maps them to its own flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read-only: plan / ask before changing anything.
    Plan,
    /// May edit files and run commands (its own permission prompts still apply).
    Build,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "plan" => Some(Mode::Plan),
            "build" => Some(Mode::Build),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Build => "build",
        }
    }
}

/// What `launch` needs to build the command line that goes into the tmux pane.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub cwd: PathBuf,
    pub mode: Mode,
    pub prompt: String,
    /// A free loopback port the adapter may use if it has an HTTP API (opencode `--port`).
    pub port: u16,
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

    /// Deliver a message to a running agent. The adapter decides the channel (HTTP API,
    /// `codex queue`, or typing into the tmux pane).
    async fn send(&self, agent: &AgentRow, text: &str) -> Result<()>;

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
