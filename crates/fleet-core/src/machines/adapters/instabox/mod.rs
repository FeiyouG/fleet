//! `instabox`: cloud machines from https://instabox.sh, over its HTTP API (no CLI needed).
//!
//! Interactive access uses the system `ssh` (or `mosh`) with a short-lived certificate fleet
//! requests for its own key pair (`ssh.rs`). Pausing is InstaBox's job (its idle rule counts
//! agent work, terminal traffic and CPU); fleet only reads and changes the settings.

pub mod api;
#[cfg(test)]
mod mock;
mod ssh;

use crate::config::MachineConfig;
use crate::machines::interfaces::{
    ExecOut, MachineAdapter, MachineRow, MachineSpec, MachineState, PausedBy, Size, META_PROFILE,
};
use anyhow::{bail, Result};
use api::{api_code, Api, Opts};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_API: &str = "https://api.instabox.sh";
pub const DEFAULT_KEY_ENV: &str = "INSTABOX_API_KEY";
/// InstaBox caps one exec at 10 minutes.
const EXEC_TIMEOUT_SECS: u64 = 600;
/// InstaBox caps an activity lease at one hour (renew it for longer).
const MAX_LEASE_SECS: u64 = 3600;

pub struct InstaBox {
    pub api: Api,
    /// Where fleet keeps its key pair and cached certificates.
    pub keys_dir: PathBuf,
    pub mosh: bool,
}

impl InstaBox {
    pub fn from_config(cfg: &MachineConfig) -> Result<Self> {
        Ok(Self {
            api: Api::new(
                cfg.api_url.as_deref().unwrap_or(DEFAULT_API),
                cfg.api_key_env.as_deref().unwrap_or(DEFAULT_KEY_ENV),
            ),
            keys_dir: crate::config::data_dir().join("instabox"),
            mosh: cfg.mosh,
        })
    }

    async fn change(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let op = self
            .api
            .call(
                method,
                path,
                body,
                Opts {
                    idempotent: true,
                    ..Opts::default()
                },
            )
            .await?;
        self.api.wait_op(&op).await
    }

    /// The machine's id from an id or a name (the API paths take ids).
    async fn resolve(&self, id_or_name: &str) -> Result<MachineRow> {
        match self.api.get(&format!("/v1/machines/{id_or_name}")).await {
            Ok(v) => Ok(to_row(&v)),
            Err(e) if api_code(&e) == Some("not_found") => {
                let all = self.list().await?;
                all.into_iter()
                    .find(|m| m.name == id_or_name)
                    .ok_or_else(|| anyhow::anyhow!("no instabox machine `{id_or_name}`"))
            }
            Err(e) => Err(e),
        }
    }

    /// Make sure the machine runs: wake an idle pause, resume a manual one, start a stopped
    /// one. A cooldown can't be skipped.
    async fn ensure_running(&self, id: &str) -> Result<MachineRow> {
        let m = self.resolve(id).await?;
        match &m.state {
            MachineState::Running => return Ok(m),
            MachineState::Paused { by: PausedBy::User } => {
                self.change(Method::POST, &format!("/v1/machines/{}/resume", m.id), None)
                    .await?;
            }
            MachineState::Paused { .. } => {
                self.change(Method::POST, &format!("/v1/machines/{}/wake", m.id), None)
                    .await?;
            }
            MachineState::Stopped => {
                self.change(Method::POST, &format!("/v1/machines/{}/start", m.id), None)
                    .await?;
            }
            MachineState::Cooldown { until } => {
                bail!("{} is cooling down until {until}; try again then", m.name)
            }
            MachineState::Starting => {}
            MachineState::Error { reason } => bail!("{} is in error: {reason}", m.name),
            MachineState::Gone => bail!("{} is deleted", m.name),
        }
        self.resolve(&m.id).await
    }
}

#[async_trait]
impl MachineAdapter for InstaBox {
    fn provider(&self) -> &'static str {
        "instabox"
    }

    async fn list(&self) -> Result<Vec<MachineRow>> {
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let path = match &cursor {
                Some(c) => format!("/v1/machines?cursor={c}"),
                None => "/v1/machines".to_string(),
            };
            let v = self.api.get(&path).await?;
            if let Some(ms) = v["machines"].as_array() {
                out.extend(ms.iter().map(to_row));
            }
            match v["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn get(&self, id: &str) -> Result<MachineRow> {
        self.resolve(id).await
    }

    async fn create(&self, spec: &MachineSpec) -> Result<MachineRow> {
        let body = create_body(spec);
        let op = self
            .api
            .call(
                Method::POST,
                "/v1/machines",
                Some(body),
                Opts {
                    idempotent: true,
                    ..Opts::default()
                },
            )
            .await?;
        let done = self.api.wait_op(&op).await?;
        let id = done["machineId"]
            .as_str()
            .or_else(|| op["machineId"].as_str())
            .map(String::from)
            .unwrap_or_else(|| spec.name.clone());
        self.resolve(&id).await
    }

    async fn destroy(&self, id: &str) -> Result<()> {
        let m = match self.resolve(id).await {
            Ok(m) => m,
            Err(e) if e.to_string().contains("no instabox machine") => return Ok(()),
            Err(e) => return Err(e),
        };
        let op = self
            .api
            .call(
                Method::DELETE,
                &format!("/v1/machines/{}", m.id),
                None,
                Opts {
                    idempotent: true,
                    headers: &[("X-Confirm-Delete", m.id.clone())],
                },
            )
            .await?;
        self.api.wait_op(&op).await.map(|_| ())
    }

    async fn exec(&self, id: &str, argv: &[String], cwd: Option<&Path>) -> Result<ExecOut> {
        let m = self.resolve(id).await?;
        let mut body = json!({ "argv": argv, "timeoutSeconds": EXEC_TIMEOUT_SECS });
        if let Some(d) = cwd {
            body["cwd"] = json!(d.to_string_lossy());
        }
        let r = self
            .api
            .call(
                Method::POST,
                &format!("/v1/machines/{}/exec", m.id),
                Some(body),
                Opts::default(),
            )
            .await?;
        Ok(ExecOut {
            code: r["exitCode"].as_i64().unwrap_or(-1) as i32,
            stdout: r["stdout"].as_str().unwrap_or_default().to_string(),
            stderr: r["stderr"].as_str().unwrap_or_default().to_string(),
        })
    }

    async fn tty_command(&self, id: &str, argv: &[String]) -> Result<Vec<String>> {
        let m = self.ensure_running(id).await?;
        ssh::tty_command(self, &m.id, argv).await
    }

    async fn pause(&self, id: &str) -> Result<()> {
        let m = self.resolve(id).await?;
        self.change(
            Method::POST,
            &format!("/v1/machines/{}/suspend", m.id),
            None,
        )
        .await
        .map(|_| ())
    }

    async fn resume(&self, id: &str) -> Result<()> {
        self.ensure_running(id).await.map(|_| ())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        let m = self.resolve(id).await?;
        self.change(Method::POST, &format!("/v1/machines/{}/stop", m.id), None)
            .await
            .map(|_| ())
    }

    async fn start(&self, id: &str) -> Result<()> {
        self.ensure_running(id).await.map(|_| ())
    }

    async fn keepalive(&self, id: &str, lease: &str, ttl: Duration) -> Result<()> {
        let m = self.resolve(id).await?;
        let secs = ttl.as_secs().clamp(30, MAX_LEASE_SECS);
        self.api
            .call(
                Method::PUT,
                &format!("/v1/machines/{}/activity-leases/{lease}", m.id),
                Some(json!({ "ttlSeconds": secs })),
                Opts::default(),
            )
            .await
            .map(|_| ())
    }

    async fn release(&self, id: &str, lease: &str) -> Result<()> {
        let m = self.resolve(id).await?;
        self.api
            .call(
                Method::DELETE,
                &format!("/v1/machines/{}/activity-leases/{lease}", m.id),
                None,
                Opts::default(),
            )
            .await
            .map(|_| ())
    }

    async fn set_idle(&self, id: &str, after: Option<Duration>) -> Result<()> {
        self.patch(
            id,
            json!({ "autoSuspendAfterSeconds": after.map(|d| d.as_secs()).unwrap_or(0) }),
        )
        .await
    }

    async fn set_max_run(&self, id: &str, after: Option<Duration>) -> Result<()> {
        let mins: i64 = after.map(|d| (d.as_secs() / 60) as i64).unwrap_or(-1);
        self.patch(id, json!({ "maxRunMinutes": mins })).await
    }

    async fn port_url(&self, id: &str, port: u16) -> Result<String> {
        let m = self.resolve(id).await?;
        let path = format!("/v1/machines/{}/ports", m.id);
        let first = self
            .api
            .call(
                Method::POST,
                &path,
                Some(json!({ "port": port, "access": "private" })),
                Opts::default(),
            )
            .await?;
        if let Some(u) = first["tokenUrl"].as_str() {
            return Ok(u.to_string());
        }
        // Already published privately: the token is only shown when made, so rotate it.
        let again = self
            .api
            .call(
                Method::POST,
                &path,
                Some(json!({ "port": port, "access": "private", "rotateToken": true })),
                Opts::default(),
            )
            .await?;
        again["tokenUrl"]
            .as_str()
            .or_else(|| again["url"].as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("instabox: no URL for port {port} on {}", m.name))
    }

    async fn publish(&self, id: &str, kv: &BTreeMap<String, String>) -> Result<()> {
        let m = self.resolve(id).await?;
        let mut meta = m.metadata.clone();
        for (k, v) in kv {
            if v.is_empty() {
                meta.remove(k);
            } else {
                meta.insert(k.clone(), v.clone());
            }
        }
        self.patch(&m.id, json!({ "metadata": meta })).await
    }
}

impl InstaBox {
    async fn patch(&self, id: &str, body: Value) -> Result<()> {
        let m = self.resolve(id).await?;
        self.api
            .call(
                Method::PATCH,
                &format!("/v1/machines/{}", m.id),
                Some(body),
                Opts::default(),
            )
            .await
            .map(|_| ())
    }
}

/// `POST /v1/machines` body from a spec. Optional settings are sent only when set: InstaBox
/// refuses (never silently replaces) a value the plan doesn't allow.
fn create_body(spec: &MachineSpec) -> Value {
    let mut b = Map::new();
    b.insert("name".into(), json!(spec.name));
    if let Some(s) = &spec.snapshot {
        b.insert("snapshotId".into(), json!(s));
    } else if let Some(t) = &spec.template {
        b.insert("templateId".into(), json!(t));
    }
    if let Some(sz) = spec.size {
        b.insert("vcpus".into(), json!(sz.vcpus));
        b.insert("memMiB".into(), json!(sz.mem_mib));
    }
    if let Some(d) = spec.idle_after {
        b.insert("autoSuspendAfterSeconds".into(), json!(d.as_secs()));
    }
    if let Some(m) = spec.max_run_min {
        b.insert("maxRunMinutes".into(), json!(m));
    }
    if !spec.secrets.is_empty() {
        b.insert("secrets".into(), json!(spec.secrets));
    }
    if !spec.files.is_empty() {
        let ds: Vec<Value> = spec
            .files
            .iter()
            .map(|(path, secret)| json!({ "secret": secret, "file": path }))
            .collect();
        b.insert("directSecrets".into(), Value::Array(ds));
    }
    if !spec.env.is_empty() {
        b.insert("env".into(), json!(spec.env));
    }
    let mut meta = spec.metadata.clone();
    if !spec.profile.is_empty() {
        meta.insert(META_PROFILE.into(), spec.profile.clone());
    }
    if !meta.is_empty() {
        b.insert("metadata".into(), json!(meta));
    }
    Value::Object(b)
}

fn ts(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// An InstaBox machine object -> fleet's row.
pub fn to_row(v: &Value) -> MachineRow {
    let now = Utc::now();
    let cooldown = ts(&v["cooldownUntil"]).filter(|t| *t > now);
    let state = match v["state"].as_str().unwrap_or("") {
        _ if cooldown.is_some() => MachineState::Cooldown {
            until: cooldown.unwrap(),
        },
        "running" => MachineState::Running,
        "creating" | "starting" | "resuming" | "suspending" | "stopping" => MachineState::Starting,
        "suspended" => MachineState::Paused {
            by: if v
                .get("autoSuspendedAt")
                .map(|x| !x.is_null())
                .unwrap_or(false)
            {
                PausedBy::Idle
            } else if !v["cooldownUntil"].is_null() {
                PausedBy::MaxRun
            } else {
                PausedBy::User
            },
        },
        "stopped" => MachineState::Stopped,
        "deleting" | "deleted" => MachineState::Gone,
        "error" => MachineState::Error {
            reason: v["lastError"]["message"]
                .as_str()
                .or_else(|| v["lastError"]["code"].as_str())
                .unwrap_or("unknown")
                .to_string(),
        },
        other => MachineState::Error {
            reason: format!("unknown state `{other}`"),
        },
    };
    let size = match (v["spec"]["vcpus"].as_u64(), v["spec"]["memMiB"].as_u64()) {
        (Some(c), Some(m)) => Some(Size {
            vcpus: c as u32,
            mem_mib: m as u32,
        }),
        _ => None,
    };
    let metadata = v["metadata"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    MachineRow {
        provider: "instabox".into(),
        id: v["id"].as_str().unwrap_or_default().to_string(),
        name: v["name"].as_str().unwrap_or_default().to_string(),
        state,
        size,
        idle_after: v["autoSuspendAfterSeconds"]
            .as_u64()
            .filter(|s| *s > 0)
            .map(Duration::from_secs),
        max_run: v["maxRunMinutes"]
            .as_i64()
            .filter(|m| *m >= 0)
            .map(|m| Duration::from_secs(m as u64 * 60)),
        oom_kills: v["oomKills"].as_u64().map(|n| n as u32),
        metadata,
        created_at: ts(&v["createdAt"]),
    }
}

#[cfg(test)]
mod tests;
