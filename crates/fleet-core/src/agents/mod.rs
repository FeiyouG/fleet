//! The agents collector family. One adapter per coding-agent CLI; opencode is the only one today.

pub mod opencode;

use crate::model::AgentRow;
use crate::procs::ProcTable;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;

/// What `launch` needs to build the command line that goes into the tmux pane.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub cwd: std::path::PathBuf,
    pub mode: String,
    pub prompt: String,
    pub port: u16,
    pub extra_args: Vec<String>,
}

/// Where to deliver a message.
#[derive(Debug, Clone)]
pub enum SendTarget {
    /// HTTP API of a fleet-started agent.
    Port(u16),
    /// tmux pane target (`sess:w.p`) of a legacy agent.
    Pane(String),
}

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    /// Every running agent of this adapter. `ports` = registry-known `pid -> port` for
    /// fleet-started agents (lets the adapter use the HTTP API).
    async fn discover(
        &self,
        procs: &ProcTable,
        known_ports: &HashMap<u32, u16>,
    ) -> Result<Vec<AgentRow>>;

    /// Deliver a message to a running agent.
    async fn send(&self, target: &SendTarget, text: &str) -> Result<()>;

    /// The shell command line that starts the agent in a tmux pane.
    fn launch(&self, spec: &LaunchSpec) -> String;

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
