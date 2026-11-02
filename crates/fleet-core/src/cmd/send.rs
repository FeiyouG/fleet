//! `fleet agent send NAME TEXT` and `fleet agent attach/stop NAME`.

use crate::agents::SendTarget;
use crate::model::{Fleet, OverviewRow};
use crate::snapshot::Engine;
use crate::tmux;
use anyhow::{bail, Context, Result};

/// Find an Overview row by worktree name, tmux session, or branch.
pub fn find_row<'a>(fleet: &'a Fleet, name: &str) -> Option<&'a OverviewRow> {
    fleet.overview.iter().find(|r| {
        r.name() == name
            || r.panes.iter().any(|p| p.session == name)
            || r.worktree.as_ref().and_then(|w| w.branch.as_deref()) == Some(name)
    })
}

/// How to reach the first agent of a row.
pub fn send_target(row: &OverviewRow) -> Option<SendTarget> {
    let agent = row.agents.first()?;
    if let Some(p) = agent.port {
        return Some(SendTarget::Port(p));
    }
    row.panes
        .iter()
        .find(|p| p.command == "opencode")
        .or_else(|| row.primary_pane())
        .map(|p| SendTarget::Pane(p.target()))
}

pub async fn send(engine: &Engine, fleet: &Fleet, name: &str, text: &str) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let target = send_target(row).with_context(|| format!("`{name}` has no running agent"))?;
    let adapter = engine
        .agent_adapter("opencode")
        .context("opencode adapter")?;
    adapter.send(&target, text).await
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
