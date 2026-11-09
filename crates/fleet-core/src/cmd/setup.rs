//! `fleet setup`: install each enabled adapter's hook/plugin, check the environment.

use crate::agents::{self, hooks, state_file, AgentAdapter, HookInstall};
use crate::config::Config;
use crate::procs::ProcTable;
use anyhow::{Context, Result};
use serde::Serialize;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct HookStatus {
    pub agent: String,
    pub path: String,
    pub installed: bool,
    pub up_to_date: bool,
    pub note: Option<&'static str>,
}

fn status_of(a: &dyn AgentAdapter) -> Option<HookStatus> {
    let note = match a.name() {
        "codex" => Some("Codex asks you to trust non-managed hooks once: run /hooks inside codex"),
        _ => None,
    };
    match a.hook_install()? {
        HookInstall::File { path, contents } => {
            let cur = std::fs::read_to_string(&path).ok();
            Some(HookStatus {
                agent: a.name().into(),
                path: path.display().to_string(),
                installed: cur.is_some(),
                up_to_date: cur.as_deref() == Some(contents),
                note,
            })
        }
        HookInstall::JsonHooks {
            path,
            events,
            command,
        } => {
            let root = hooks::read_settings(&path).unwrap_or(serde_json::json!({}));
            let ok = hooks::hooks_installed(&root, events, &command);
            Some(HookStatus {
                agent: a.name().into(),
                path: path.display().to_string(),
                installed: ok,
                up_to_date: ok,
                note,
            })
        }
    }
}

/// Install hooks for every enabled adapter. Returns one status per adapter.
pub fn install(adapters: &[Box<dyn AgentAdapter>]) -> Result<Vec<HookStatus>> {
    std::fs::create_dir_all(state_file::dir())?;
    let mut out = vec![];
    for a in adapters {
        match a.hook_install() {
            None => continue,
            Some(HookInstall::File { path, contents }) => {
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, contents)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            Some(HookInstall::JsonHooks {
                path,
                events,
                command,
            }) => {
                let mut root = hooks::read_settings(&path)?;
                hooks::merge_hooks(&mut root, events, &command);
                hooks::write_settings(&path, &root)?;
            }
        }
        out.extend(status_of(a.as_ref()));
    }
    Ok(out)
}

/// Remove fleet's hooks for every adapter (enabled or not). Returns what was removed.
pub fn remove() -> Result<Vec<String>> {
    let mut removed = vec![];
    for name in agents::KNOWN {
        let Some(a) = agents::adapter_for(name, Default::default()) else {
            continue;
        };
        match a.hook_install() {
            None => {}
            Some(HookInstall::File { path, .. }) => {
                if std::fs::remove_file(&path).is_ok() {
                    removed.push(format!("{name}: {}", path.display()));
                }
            }
            Some(HookInstall::JsonHooks { path, command, .. }) => {
                if !path.exists() {
                    continue;
                }
                let mut root = hooks::read_settings(&path)?;
                if hooks::unmerge_hooks(&mut root, &command) {
                    hooks::write_settings(&path, &root)?;
                    removed.push(format!("{name}: {}", path.display()));
                }
            }
        }
    }
    Ok(removed)
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub hooks: Vec<HookStatus>,
    pub agents: Vec<AgentCheck>,
    pub tools: Vec<ToolCheck>,
}

#[derive(Debug, Serialize)]
pub struct AgentCheck {
    pub agent: String,
    pub bin_found: bool,
    pub running: usize,
    pub reporting: usize,
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

pub async fn check(cfg: &Config, adapters: &[Box<dyn AgentAdapter>]) -> Check {
    let procs = ProcTable::snapshot();
    let states = state_file::read_all(&procs);
    let mut agent_checks = vec![];
    for a in adapters {
        let bin = cfg
            .enabled_agents()
            .into_iter()
            .find(|x| x.harness == a.name())
            .map(|x| x.bin)
            .unwrap_or_else(|| a.name().to_string());
        let bin_found = which(&bin).await;
        let pids: Vec<u32> = a
            .process_names()
            .iter()
            .flat_map(|n| procs.named(n).into_iter().map(|p| p.pid))
            .collect();
        let reporting = pids
            .iter()
            .filter(|p| {
                states
                    .get(p)
                    .map(|s| s.agent.is_empty() || s.agent == a.name())
                    .unwrap_or(false)
            })
            .count();
        agent_checks.push(AgentCheck {
            agent: a.name().into(),
            bin_found,
            running: pids.len(),
            reporting,
        });
    }
    let hooks = adapters
        .iter()
        .filter_map(|a| status_of(a.as_ref()))
        .collect();
    let mut tools = vec![
        tool("tmux", &["-V"], "sessions and attach").await,
        tool("git", &["--version"], "worktrees").await,
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
        hooks,
        agents: agent_checks,
        tools,
    }
}

async fn which(bin: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {}", shell_words::quote(bin))])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub const TMUX_SNIPPET: &str =
    "set -g status-interval 15\nset -g status-right '#(fleet status --short) | %H:%M'";
