//! `fleet agent send NAME TEXT` and `fleet agent attach/stop NAME`.

use crate::agents::{AgentAdapter, SendMsg, SendTarget};
use crate::model::{AgentRow, Fleet, OverviewRow};
use crate::snapshot::Engine;
use crate::tmux;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Find an Overview row by worktree name, tmux session, or branch.
pub fn find_row<'a>(fleet: &'a Fleet, name: &str) -> Option<&'a OverviewRow> {
    fleet.overview.iter().find(|r| {
        r.name() == name
            || r.panes.iter().any(|p| p.session == name)
            || r.worktree.as_ref().and_then(|w| w.branch.as_deref()) == Some(name)
    })
}

/// How to reach one agent: the fleet plugin's control endpoint, else its tmux pane.
pub fn agent_target(agent: &AgentRow) -> Option<SendTarget> {
    if let (Some(url), Some(token)) = (&agent.control_url, &agent.control_token) {
        return Some(SendTarget::Control {
            url: url.clone(),
            token: token.clone(),
        });
    }
    agent.pane_target.clone().map(SendTarget::Pane)
}

/// How to reach the first agent of a row.
pub fn send_target(row: &OverviewRow) -> Option<SendTarget> {
    let agent = row.agents.first()?;
    agent_target(agent).or_else(|| {
        row.panes
            .iter()
            .find(|p| p.command == "opencode")
            .or_else(|| row.primary_pane())
            .map(|p| SendTarget::Pane(p.target()))
    })
}

/// Check a model/agent override against what the agent's CLI offers in its directory.
pub async fn check_override(adapter: &dyn AgentAdapter, cwd: &Path, msg: &SendMsg) -> Result<()> {
    if !msg.has_override() {
        return Ok(());
    }
    let c = adapter.choices(cwd).await?;
    if let Some(m) = &msg.model {
        c.check_model(m)?;
    }
    if let Some(a) = &msg.agent {
        c.check_agent(a)?;
    }
    Ok(())
}

pub async fn send(engine: &Engine, fleet: &Fleet, name: &str, msg: &SendMsg) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let target = send_target(row).with_context(|| format!("`{name}` has no running agent"))?;
    let adapter = engine
        .agent_adapter("opencode")
        .context("opencode adapter")?;
    if let Some(a) = row.agents.first() {
        check_override(adapter, &a.cwd, msg).await?;
    }
    adapter.send(&target, msg).await
}

pub async fn attach(fleet: &Fleet, name: &str) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let pane = row
        .primary_pane()
        .with_context(|| format!("`{name}` has no tmux pane"))?;
    tmux::attach(&pane.target()).await
}

pub async fn stop(fleet: &Fleet, name: &str) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let pane = row
        .panes
        .iter()
        .find(|p| p.command == "opencode")
        .context("no opencode pane to stop")?;
    if row.agents.is_empty() {
        bail!("`{name}` has no running agent");
    }
    tmux::kill_pane(&pane.target()).await
}
