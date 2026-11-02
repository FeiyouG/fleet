//! State files written by the fleet opencode plugin (`assets/fleet.ts`):
//! `~/.local/share/fleet/agents/<pid>.json`.

use crate::config::data_dir;
use crate::procs::ProcTable;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub const PLUGIN_SOURCE: &str = include_str!("../../../assets/fleet.ts");

#[derive(Debug, Clone, Deserialize)]
pub struct Pending {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub pending: Vec<Pending>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentState {
    pub pid: u32,
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub directory: PathBuf,
    #[serde(default)]
    pub sessions: HashMap<String, SessionState>,
    #[serde(default)]
    pub updated_at: i64,
    /// Plugin v2: control endpoint (`http://127.0.0.1:N`) and its bearer token.
    #[serde(default)]
    pub control_url: String,
    #[serde(default)]
    pub token: String,
    /// Plugin v2: the root (non-subagent) session the user last worked in.
    #[serde(default)]
    pub current_session: String,
}

impl AgentState {
    /// The session this process is showing: the one with pending permissions, else the
    /// one the plugin reports as current, else the busiest, else the first.
    pub fn primary_session(&self) -> Option<(&String, &SessionState)> {
        self.sessions.iter().max_by_key(|(id, s)| {
            (
                !s.pending.is_empty(),
                **id == self.current_session,
                s.status == "busy",
            )
        })
    }

    /// `(url, token)` of the plugin's control endpoint, when it has one.
    pub fn control(&self) -> Option<(String, String)> {
        (!self.control_url.is_empty() && !self.token.is_empty())
            .then(|| (self.control_url.clone(), self.token.clone()))
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
}

pub fn dir() -> PathBuf {
    data_dir().join("agents")
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
        assert_eq!(st.control(), None);
    }

    #[test]
    fn v2_control_and_current_session() {
        let st = parse(
            r#"{"version":2,"pid":42,"server_url":"http://localhost:4096/","control_url":"http://127.0.0.1:52965",
                "token":"tok","directory":"/w","current_session":"ses_root","sessions":{
                "ses_root":{"status":"busy","pending":[],"agent":"build"},
                "ses_child":{"status":"busy","parent":"ses_root","pending":[]}
            },"updated_at":1}"#,
        )
        .unwrap();
        assert_eq!(
            st.control(),
            Some(("http://127.0.0.1:52965".into(), "tok".into()))
        );
        assert_eq!(st.primary_session().unwrap().0, "ses_root");
    }
}
