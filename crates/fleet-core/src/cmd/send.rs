//! `fleet agent send NAME TEXT` and `fleet agent attach/stop NAME`.

use super::Progress;
use crate::agents::{AgentAdapter, SendMsg};
use crate::model as fleet_agent;
use crate::model::{Fleet, OverviewRow};
use crate::snapshot::Engine;
use crate::tmux;
use anyhow::{Context, Result};
use std::path::Path;

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

/// Check a mode/model override against what the agent's adapter offers in its directory.
/// Same rule as `start`: a wrong name is an error, but if discovery itself fails (CLI slow
/// or missing) the message goes ahead with a warning and the agent CLI judges the name.
pub async fn check_override(
    adapter: &dyn AgentAdapter,
    cwd: &Path,
    msg: &SendMsg,
    log: &Progress,
) -> Result<()> {
    if !msg.has_override() {
        return Ok(());
    }
    match adapter.choices(cwd).await {
        Ok(c) => c.check(msg.mode.as_deref(), msg.model.as_deref()),
        Err(e) => {
            log(&format!(
                "could not list modes/models ({e:#}); sending unchecked"
            ));
            Ok(())
        }
    }
}

pub async fn send(
    engine: &Engine,
    fleet: &Fleet,
    name: &str,
    msg: &SendMsg,
    log: &Progress,
) -> Result<()> {
    let row = find_row(fleet, name).with_context(|| format!("no agent/worktree named `{name}`"))?;
    let agent = first_agent(row).with_context(|| format!("`{name}` has no running agent"))?;
    let adapter = engine
        .agent_adapter(&agent.adapter)
        .with_context(|| format!("agent `{}` is not enabled", agent.adapter))?;
    check_override(adapter, &agent.cwd, msg, log).await?;
    adapter.send(&agent, msg).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Choices, DiscoverCtx, HookInstall, LaunchSpec};
    use crate::model::AgentRow;
    use crate::procs::ProcTable;
    use std::sync::{Arc, Mutex};

    /// An adapter whose discovery either fails or offers plan/build + one model.
    struct Stub(bool);

    #[async_trait::async_trait]
    impl AgentAdapter for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn process_names(&self) -> &'static [&'static str] {
            &[]
        }
        async fn discover(&self, _: &ProcTable, _: &DiscoverCtx) -> Result<Vec<AgentRow>> {
            Ok(vec![])
        }
        fn launch(&self, _: &LaunchSpec) -> String {
            String::new()
        }
        async fn send(&self, _: &AgentRow, _: &SendMsg) -> Result<()> {
            Ok(())
        }
        async fn choices(&self, _: &Path) -> Result<Choices> {
            if self.0 {
                anyhow::bail!("`opencode models` timed out")
            }
            Ok(Choices {
                models: vec!["p/m".into()],
                ..Choices::plan_build()
            })
        }
        fn hook_install(&self) -> Option<HookInstall> {
            None
        }
    }

    fn msg(mode: Option<&str>, model: Option<&str>) -> SendMsg {
        SendMsg {
            text: "hi".into(),
            mode: mode.map(String::from),
            model: model.map(String::from),
        }
    }

    #[tokio::test]
    async fn override_checked_but_discovery_failure_does_not_block() {
        let logs = Arc::new(Mutex::new(vec![]));
        let l = logs.clone();
        let log: Progress = Box::new(move |s| l.lock().unwrap().push(s.to_string()));
        let ok = Stub(false);
        let cwd = Path::new("/w");
        assert!(
            check_override(&ok, cwd, &msg(Some("build"), Some("p/m")), &log)
                .await
                .is_ok()
        );
        assert!(check_override(&ok, cwd, &msg(None, Some("p/x")), &log)
            .await
            .is_err());
        assert!(check_override(&ok, cwd, &msg(Some("reviewer"), None), &log)
            .await
            .is_err());
        assert!(logs.lock().unwrap().is_empty());
        // discovery fails: the message still goes, with a warning
        let failing = Stub(true);
        assert!(check_override(&failing, cwd, &msg(None, Some("p/x")), &log)
            .await
            .is_ok());
        assert!(logs.lock().unwrap()[0].contains("sending unchecked"));
    }
}
