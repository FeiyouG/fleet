//! The machine interface: what fleet needs from any computer an agent runs on.
//!
//! Every tmux and git command fleet runs goes through [`MachineAdapter::exec`] (or, with a
//! terminal, [`MachineAdapter::tty_command`]), so a session and a worktree are the same code on
//! the laptop and on a cloud machine. Listing machines must never wake a paused one.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// A verb this provider has no way to do (e.g. `pause` on the laptop). Callers report it as
/// "not supported", never as a failure of the machine.
#[derive(Debug, thiserror::Error)]
#[error("the `{provider}` machine adapter does not support `{verb}`")]
pub struct Unsupported {
    pub provider: &'static str,
    pub verb: &'static str,
}

pub fn unsupported<T>(provider: &'static str, verb: &'static str) -> Result<T> {
    Err(Unsupported { provider, verb }.into())
}

/// Is this error an [`Unsupported`] verb?
pub fn is_unsupported(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unsupported>().is_some()
}

/// Who paused a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PausedBy {
    /// The provider's idle rule.
    Idle,
    /// The provider's run cap (`max_run`).
    MaxRun,
    /// You (or fleet on your behalf).
    User,
}

/// A machine's state (spec: machines, "Machine states").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum MachineState {
    /// Up and billing compute.
    Running,
    /// Creating, starting or resuming.
    Starting,
    /// Frozen with memory kept; disk-only billing. Attaching wakes it.
    Paused { by: PausedBy },
    /// Processes ended, disk kept; needs `start`.
    Stopped,
    /// Can't wake until `until` (after a run-cap pause).
    Cooldown { until: DateTime<Utc> },
    /// The provider reports a failure.
    Error { reason: String },
    /// Being deleted or deleted.
    Gone,
}

impl MachineState {
    pub fn is_running(&self) -> bool {
        matches!(self, MachineState::Running)
    }
    pub fn label(&self) -> &'static str {
        match self {
            MachineState::Running => "running",
            MachineState::Starting => "starting",
            MachineState::Paused { .. } => "paused",
            MachineState::Stopped => "stopped",
            MachineState::Cooldown { .. } => "cooldown",
            MachineState::Error { .. } => "error",
            MachineState::Gone => "gone",
        }
    }
}

/// vCPUs and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub vcpus: u32,
    pub mem_mib: u32,
}

impl Size {
    /// `4/8` (vCPUs / GiB).
    pub fn label(&self) -> String {
        format!("{}/{}", self.vcpus, self.mem_mib / 1024)
    }
}

/// One machine as fleet sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRow {
    /// `local` | `instabox`.
    pub provider: String,
    /// The provider's id (`laptop` for the local machine, `ibx_…` on InstaBox).
    pub id: String,
    /// The name shown everywhere (`laptop`, `ibx-fix-a`).
    pub name: String,
    pub state: MachineState,
    pub size: Option<Size>,
    /// Pauses after this much idle time; `None` = never (or not applicable).
    pub idle_after: Option<Duration>,
    /// Pauses after this much continuous running; `None` = unlimited.
    pub max_run: Option<Duration>,
    /// Out-of-memory kills since creation, when the provider reports them.
    pub oom_kills: Option<u32>,
    /// String labels kept by the provider (fleet stores the profile and agent state here).
    pub metadata: BTreeMap<String, String>,
    pub created_at: Option<DateTime<Utc>>,
}

impl MachineRow {
    /// The machine profile that created it (`fleet.profile` metadata), if fleet made it.
    pub fn profile(&self) -> Option<&str> {
        self.metadata.get(META_PROFILE).map(String::as_str)
    }
}

/// Metadata key holding the machine profile that created a machine.
pub const META_PROFILE: &str = "fleet.profile";

/// Everything needed to create a machine from a profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineSpec {
    pub name: String,
    /// The machine profile (stored in metadata so fleet can find its machines again).
    pub profile: String,
    pub template: Option<String>,
    pub snapshot: Option<String>,
    pub size: Option<Size>,
    /// Pause after this much idle time; zero = never. `None` = the provider's default.
    pub idle_after: Option<Duration>,
    /// Pause after this many running minutes; -1 = unlimited. `None` = provider default.
    pub max_run_min: Option<i64>,
    /// Proxy secrets, by name: the machine sees a placeholder, the provider swaps the value.
    pub secrets: Vec<String>,
    /// Direct secrets delivered as files: home-relative path -> secret name.
    pub files: BTreeMap<String, String>,
    /// Plain environment variables for every login shell.
    pub env: BTreeMap<String, String>,
    /// Extra metadata (merged with the profile key).
    pub metadata: BTreeMap<String, String>,
}

/// A command's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ExecOut {
    pub fn success(&self) -> bool {
        self.code == 0
    }
    /// `Ok(stdout)` on exit 0, else an error carrying stderr.
    pub fn ok(self, what: &str) -> Result<String> {
        if self.success() {
            Ok(self.stdout)
        } else {
            anyhow::bail!("{what} failed ({}): {}", self.code, self.stderr.trim())
        }
    }
}

#[async_trait]
pub trait MachineAdapter: Send + Sync {
    /// `local` | `instabox`.
    fn provider(&self) -> &'static str;

    /// Every machine of this adapter (for `instabox`: of the API key's workspace). Must
    /// never wake a paused machine.
    async fn list(&self) -> Result<Vec<MachineRow>>;

    /// One machine by id or name.
    async fn get(&self, id: &str) -> Result<MachineRow>;

    /// Create a machine and wait until it runs.
    async fn create(&self, spec: &MachineSpec) -> Result<MachineRow>;

    /// Delete a machine and everything on it.
    async fn destroy(&self, id: &str) -> Result<()>;

    /// Run `argv` (no shell) in the machine, optionally in `cwd`, and capture its output.
    async fn exec(&self, id: &str, argv: &[String], cwd: Option<&Path>) -> Result<ExecOut>;

    /// The local argv that runs `argv` in the machine with a terminal (attach, shell, diff).
    /// Wakes a paused machine first.
    async fn tty_command(&self, id: &str, argv: &[String]) -> Result<Vec<String>>;

    async fn pause(&self, _id: &str) -> Result<()> {
        unsupported(self.provider(), "pause")
    }
    async fn resume(&self, _id: &str) -> Result<()> {
        unsupported(self.provider(), "resume")
    }
    async fn stop(&self, _id: &str) -> Result<()> {
        unsupported(self.provider(), "stop")
    }
    async fn start(&self, _id: &str) -> Result<()> {
        unsupported(self.provider(), "start")
    }
    /// Keep the machine awake for `ttl` under the lease name `lease`.
    async fn keepalive(&self, _id: &str, _lease: &str, _ttl: Duration) -> Result<()> {
        unsupported(self.provider(), "keep")
    }
    async fn release(&self, _id: &str, _lease: &str) -> Result<()> {
        unsupported(self.provider(), "release")
    }
    /// Pause after `after` idle time; `None` = never.
    async fn set_idle(&self, _id: &str, _after: Option<Duration>) -> Result<()> {
        unsupported(self.provider(), "idle")
    }
    /// Pause after `after` continuous running; `None` = unlimited.
    async fn set_max_run(&self, _id: &str, _after: Option<Duration>) -> Result<()> {
        unsupported(self.provider(), "max-run")
    }
    /// A link to `port` in the machine that only you can open.
    async fn port_url(&self, _id: &str, _port: u16) -> Result<String> {
        unsupported(self.provider(), "port url")
    }
    /// Merge `kv` into the machine's metadata; an empty value removes that key. Other keys
    /// are kept.
    async fn publish(&self, _id: &str, _kv: &BTreeMap<String, String>) -> Result<()> {
        unsupported(self.provider(), "publish")
    }
}
