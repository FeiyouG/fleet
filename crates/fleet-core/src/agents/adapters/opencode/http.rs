//! The opencode HTTP API (when its port actually listens; opencode 1.18 TUIs may not), and
//! the fleet plugin's control endpoint.

use crate::agents::SendMsg;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .expect("reqwest client")
}

#[derive(Debug, Clone, Default)]
pub struct Live {
    /// Session id -> status string (`busy`, `idle`, ...).
    pub busy: bool,
    pub pending_permission: bool,
}

/// `GET /session/status` -> `{ "<id>": {"type": "busy"|"idle"|...}, ... }`.
pub async fn live(port: u16) -> Option<Live> {
    let v: Value = client()
        .get(format!("http://127.0.0.1:{port}/session/status"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let mut live = Live::default();
    if let Some(obj) = v.as_object() {
        for (_, st) in obj {
            let t = st.get("type").and_then(Value::as_str).unwrap_or("");
            if t == "busy" || t == "running" {
                live.busy = true;
            }
            if t == "permission" || st.get("permission").is_some() {
                live.pending_permission = true;
            }
        }
    }
    Some(live)
}

/// Is a fleet-started opencode listening on this port?
pub async fn is_opencode(port: u16) -> bool {
    client()
        .get(format!("http://127.0.0.1:{port}/path"))
        .send()
        .await
        .ok()
        .filter(|r| r.status().is_success())
        .is_some()
}

/// `POST <control_url>/send` on the fleet plugin's endpoint (`assets/fleet.ts`). A plain
/// message is typed into the TUI; one with a model/agent goes through the session API.
pub async fn control_send(url: &str, token: &str, msg: &SendMsg) -> Result<()> {
    let r = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .post(format!("{}/send", url.trim_end_matches('/')))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "text": msg.text,
            "model": msg.model,
            // fleet's mode = opencode's agent
            "agent": msg.mode,
        }))
        .send()
        .await
        .context("fleet plugin control endpoint")?;
    let status = r.status();
    let body: Value = r.json().await.unwrap_or(Value::Null);
    if !status.is_success() || body.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("no details");
        bail!("agent rejected the message ({status}): {err}");
    }
    Ok(())
}
