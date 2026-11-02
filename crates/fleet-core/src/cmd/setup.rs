//! `fleet setup`: install the opencode plugin, check the environment.

use crate::agents::opencode::state_file;
use crate::config::{home, Config};
use crate::procs::ProcTable;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::PathBuf;
use tokio::process::Command;

pub fn plugin_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("opencode")
        .join("plugins")
        .join("fleet.ts")
}

pub fn install() -> Result<PathBuf> {
    let p = plugin_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, state_file::PLUGIN_SOURCE)
        .with_context(|| format!("writing {}", p.display()))?;
    std::fs::create_dir_all(state_file::dir())?;
    Ok(p)
}

pub fn remove() -> Result<bool> {
    let p = plugin_path();
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", p.display())),
    }
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub plugin_path: String,
    pub plugin_installed: bool,
    pub plugin_up_to_date: bool,
    pub agents_running: usize,
    pub agents_with_state: usize,
    pub tools: Vec<ToolCheck>,
}

#[derive(Debug, Serialize)]
pub struct ToolCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

async fn tool(name: &str, args: &[&str], needed_by: &str) -> ToolCheck {
    match Command::new(name).args(args).output().await {
        Ok(o) if o.status.success() => ToolCheck {
            name: name.into(),
            ok: true,
            detail: needed_by.into(),
        },
        Ok(o) => ToolCheck {
            name: name.into(),
            ok: false,
            detail: format!(
                "{needed_by}: {}",
                String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or("failed")
            ),
        },
        Err(_) => ToolCheck {
            name: name.into(),
            ok: false,
            detail: format!("{needed_by}: not installed"),
        },
    }
}

pub async fn check(cfg: &Config) -> Check {
    let p = plugin_path();
    let installed = p.exists();
    let up_to_date = installed
        && std::fs::read_to_string(&p)
            .map(|s| s == state_file::PLUGIN_SOURCE)
            .unwrap_or(false);
    let procs = ProcTable::snapshot();
    let running = procs.named("opencode").len();
    let with_state = state_file::read_all(&procs).len();
    let mut tools = vec![
        tool("tmux", &["-V"], "sessions and attach").await,
        tool("git", &["--version"], "worktrees").await,
        tool("opencode", &["--version"], "the agent").await,
    ];
    if cfg.projects.iter().any(|p| {
        p.reviews
            .as_deref()
            .map(|s| s.starts_with("gitlab:"))
            .unwrap_or(false)
    }) {
        tools.push(tool("glab", &["auth", "status"], "GitLab reviews").await);
    }
    if cfg.projects.iter().any(|p| {
        p.reviews
            .as_deref()
            .map(|s| s.starts_with("github:"))
            .unwrap_or(false)
    }) {
        tools.push(tool("gh", &["auth", "status"], "GitHub reviews").await);
    }
    Check {
        plugin_path: p.display().to_string(),
        plugin_installed: installed,
        plugin_up_to_date: up_to_date,
        agents_running: running,
        agents_with_state: with_state,
        tools,
    }
}

pub const TMUX_SNIPPET: &str =
    "set -g status-interval 15\nset -g status-right '#(fleet status --short) | %H:%M'";
