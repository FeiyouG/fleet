//! `local`: this computer. Always one machine, always running; commands run as local
//! processes exactly as fleet ran them before machines existed.

use crate::machines::interfaces::{
    unsupported, ExecOut, MachineAdapter, MachineRow, MachineSpec, MachineState,
};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::Path;
use tokio::process::Command;

pub struct Local {
    /// The machine profile's name, also the machine's id and name (`laptop`).
    name: String,
}

impl Local {
    pub fn new(name: &str) -> Self {
        Self { name: name.into() }
    }

    fn row(&self) -> MachineRow {
        MachineRow {
            provider: "local".into(),
            id: self.name.clone(),
            name: self.name.clone(),
            state: MachineState::Running,
            size: None,
            idle_after: None,
            max_run: None,
            oom_kills: None,
            cost_usd: None,
            metadata: BTreeMap::new(),
            created_at: None,
        }
    }

    fn check(&self, id: &str) -> Result<()> {
        if id != self.name {
            bail!(
                "no local machine `{id}` (the local machine is `{}`)",
                self.name
            );
        }
        Ok(())
    }
}

#[async_trait]
impl MachineAdapter for Local {
    fn provider(&self) -> &'static str {
        "local"
    }

    async fn list(&self) -> Result<Vec<MachineRow>> {
        Ok(vec![self.row()])
    }

    async fn get(&self, id: &str) -> Result<MachineRow> {
        self.check(id)?;
        Ok(self.row())
    }

    async fn create(&self, _spec: &MachineSpec) -> Result<MachineRow> {
        // The laptop always exists: "creating" it is just using it.
        Ok(self.row())
    }

    async fn destroy(&self, _id: &str) -> Result<()> {
        unsupported("local", "rm")
    }

    async fn exec(&self, id: &str, argv: &[String], cwd: Option<&Path>) -> Result<ExecOut> {
        self.check(id)?;
        let (prog, args) = argv.split_first().context("exec: empty command")?;
        let mut cmd = Command::new(prog);
        cmd.args(args);
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let out = cmd
            .output()
            .await
            .with_context(|| format!("running {prog}"))?;
        Ok(ExecOut {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    async fn tty_command(&self, id: &str, argv: &[String]) -> Result<Vec<String>> {
        self.check(id)?;
        Ok(argv.to_vec())
    }

    async fn port_url(&self, id: &str, port: u16) -> Result<String> {
        self.check(id)?;
        Ok(format!("http://127.0.0.1:{port}"))
    }

    /// Local agent state already lives in local state files; nothing to publish.
    async fn publish(&self, id: &str, _kv: &BTreeMap<String, String>) -> Result<()> {
        self.check(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::interfaces::is_unsupported;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn exec_runs_locally_in_cwd() {
        let m = Local::new("laptop");
        let dir = std::env::temp_dir();
        let out = m.exec("laptop", &argv(&["pwd"]), Some(&dir)).await.unwrap();
        assert!(out.success());
        let got = std::path::PathBuf::from(out.stdout.trim())
            .canonicalize()
            .unwrap();
        assert_eq!(got, dir.canonicalize().unwrap());
        let bad = m.exec("laptop", &argv(&["false"]), None).await.unwrap();
        assert_eq!(bad.code, 1);
        assert!(bad.ok("false").is_err());
    }

    #[tokio::test]
    async fn tty_is_argv_unchanged_and_one_running_machine() {
        let m = Local::new("laptop");
        let a = argv(&["tmux", "attach", "-t", "fix-a"]);
        assert_eq!(m.tty_command("laptop", &a).await.unwrap(), a);
        let l = m.list().await.unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, "laptop");
        assert!(l[0].state.is_running());
        assert!(m.get("ibx-x").await.is_err());
    }

    #[tokio::test]
    async fn lifecycle_verbs_unsupported() {
        let m = Local::new("laptop");
        assert!(is_unsupported(&m.pause("laptop").await.unwrap_err()));
        assert!(is_unsupported(&m.destroy("laptop").await.unwrap_err()));
        assert!(is_unsupported(
            &m.keepalive("laptop", "x", std::time::Duration::from_secs(1))
                .await
                .unwrap_err()
        ));
    }
}
