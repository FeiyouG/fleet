//! State files: `~/.local/share/fleet/agents/<pid>.json`, one per running agent process.
//!
//! Written by the opencode plugin (`assets/fleet.ts`) or by `fleet agent-hook <agent>`
//! (Claude Code / Codex hooks). Read by discovery. The format is agent-neutral: it holds
//! exactly the facts an agent's own storage never records — pending permission prompts,
//! the session being shown, busy/idle — plus what helps identify it.

use crate::config::data_dir;
use crate::procs::ProcTable;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const PLUGIN_SOURCE: &str = include_str!("../../assets/fleet.ts");

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pending {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub pending: Vec<Pending>,
    /// The agent is blocked on a question to the user (Claude `AskUserQuestion`, …).
    #[serde(default)]
    pub question: bool,
    /// The agent presented a plan and waits for approval (Claude `ExitPlanMode`).
    #[serde(default)]
    pub plan: bool,
    /// Agent-reported mode (`plan`, `build`, `default`, `acceptEdits`, …).
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    #[serde(default)]
    pub version: u32,
    pub pid: u32,
    /// Adapter name: `opencode`, `claude`, `codex`.
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub directory: PathBuf,
    #[serde(default)]
    pub sessions: HashMap<String, SessionState>,
    #[serde(default)]
    pub updated_at: i64,
}

impl AgentState {
    /// The session this process is showing: the one with something pending, else the
    /// busiest, else the most recently titled, else any.
    pub fn primary_session(&self) -> Option<(&String, &SessionState)> {
        self.sessions.iter().max_by_key(|(_, s)| {
            (
                !s.pending.is_empty() || s.question || s.plan,
                s.status == "busy",
                s.title.is_some(),
            )
        })
    }

    pub fn pending(&self) -> Vec<&Pending> {
        self.sessions
            .values()
            .flat_map(|s| s.pending.iter())
            .collect()
    }

    pub fn port(&self) -> Option<u16> {
        self.server_url
            .rsplit(':')
            .next()?
            .trim_end_matches('/')
            .parse()
            .ok()
    }

    pub fn session_mut(&mut self, id: &str) -> &mut SessionState {
        self.sessions.entry(id.to_string()).or_default()
    }
}

pub fn dir() -> PathBuf {
    data_dir().join("agents")
}

pub fn path_for(pid: u32) -> PathBuf {
    dir().join(format!("{pid}.json"))
}

/// Read every state file; delete the ones whose process is gone.
pub fn read_all(procs: &ProcTable) -> HashMap<u32, AgentState> {
    let mut out = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(st) = serde_json::from_str::<AgentState>(&text) else {
            continue;
        };
        if !procs.is_alive(st.pid) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        out.insert(st.pid, st);
    }
    out
}

pub fn parse(text: &str) -> Option<AgentState> {
    serde_json::from_str(text).ok()
}

/// Read-modify-write one state file under an advisory lock (hooks can fire concurrently).
pub fn update(pid: u32, f: impl FnOnce(&mut AgentState)) -> Result<()> {
    let path = path_for(pid);
    std::fs::create_dir_all(dir())?;
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    flock(&lock)?;
    let mut st = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| parse(&t))
        .unwrap_or_default();
    st.pid = pid;
    st.version = 1;
    f(&mut st);
    st.updated_at = chrono::Utc::now().timestamp_millis();
    write_atomic(&path, &serde_json::to_vec(&st)?)
}

pub fn remove(pid: u32) {
    let _ = std::fs::remove_file(path_for(pid));
    let _ = std::fs::remove_file(path_for(pid).with_extension("lock"));
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

#[cfg(unix)]
fn flock(f: &std::fs::File) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    // libc flock without pulling the libc crate: tokio/sysinfo already link it, but keep
    // this self-contained via std's FileExt where available.
    extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    let rc = unsafe { flock(f.as_raw_fd(), LOCK_EX) };
    if rc != 0 {
        anyhow::bail!("flock failed");
    }
    Ok(())
}

#[cfg(not(unix))]
fn flock(_f: &std::fs::File) -> Result<()> {
    Ok(())
}

/// Tests that set `FLEET_DATA_DIR` must not run concurrently.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_picks_session() {
        let st = parse(
            r#"{"version":1,"pid":42,"server_url":"http://127.0.0.1:61234/","directory":"/w","sessions":{
                "ses_a":{"status":"idle","pending":[]},
                "ses_b":{"status":"busy","pending":[{"id":"per_1","title":"bash: rm -rf dist","type":"bash"}]}
            },"updated_at":1}"#,
        )
        .unwrap();
        assert_eq!(st.port(), Some(61234));
        assert_eq!(st.primary_session().unwrap().0, "ses_b");
        assert_eq!(st.pending().len(), 1);
        assert_eq!(st.pending()[0].title, "bash: rm -rf dist");
        assert_eq!(st.agent, "");
    }

    #[test]
    fn update_roundtrip() {
        let _g = test_lock();
        let tmp = std::env::temp_dir().join(format!("fleet-sf-{}", std::process::id()));
        std::env::set_var("FLEET_DATA_DIR", &tmp);
        update(999_999, |s| {
            s.agent = "claude".into();
            s.session_mut("abc").status = "busy".into();
        })
        .unwrap();
        update(999_999, |s| {
            s.session_mut("abc").pending.push(Pending {
                title: "Bash: rm".into(),
                ..Default::default()
            })
        })
        .unwrap();
        let st = parse(&std::fs::read_to_string(path_for(999_999)).unwrap()).unwrap();
        assert_eq!(st.agent, "claude");
        assert_eq!(st.sessions["abc"].status, "busy");
        assert_eq!(st.pending().len(), 1);
        remove(999_999);
        assert!(!path_for(999_999).exists());
        std::fs::remove_dir_all(&tmp).ok();
        std::env::remove_var("FLEET_DATA_DIR");
    }
}
