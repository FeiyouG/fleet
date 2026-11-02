//! `fleet agent send NAME TEXT` and `fleet agent attach/stop NAME`.

use crate::model as fleet_agent;
use crate::model::{Fleet, OverviewRow};
use crate::snapshot::Engine;
use crate::tmux;
use anyhow::{Context, Result};

/// Find an Overview row by worktree name, tmux session, or branch.
pub fn find_row<'a>(fleet: &'a Fleet, name: &str) -> Option<&'a OverviewRow> {
    fleet.overview.iter().find(|r| {
        r.name() == name
            || r.panes.iter().any(|p| p.session == name)
            || r.worktree.as_ref().and_then(|w| w.branch.as_deref()) == Some(name)
    })
}

/// The first agent of a row, with its pane target filled in.
pub fn first_agent(row: &OverviewRow) -> Option<fleet_agent::AgentRow> {
    let mut a = row.agents.first()?.clone();
    if a.pane_target.is_none() {
        a.pane_target = row
            .panes
            .iter()
            .find(|p| p.command == a.adapter)
            .or_else(|| row.primary_pane())
            .map(|p| p.target());
    }
    Some(a)
}

pub async fn send(engine: &Engine, fleet: &Fleet, name: &str, text: &str) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let agent = first_agent(row).with_context(|| format!("`{name}` has no running agent"))?;
    let adapter = engine
        .agent_adapter(&agent.adapter)
        .with_context(|| format!("agent `{}` is not enabled", agent.adapter))?;
    adapter.send(&agent, text).await
}

pub async fn attach(fleet: &Fleet, name: &str) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let pane = row
        .primary_pane()
        .with_context(|| format!("`{name}` has no tmux pane"))?;
    tmux::attach(&pane.target()).await
}

/// Stop the row's (first) agent: kill its pane, and wait until the process has exited.
pub async fn stop(fleet: &Fleet, name: &str) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let agent = first_agent(row).with_context(|| format!("`{name}` has no running agent"))?;
    super::txn::stop_pid(agent.pid, agent.pane_target.as_deref()).await
}
