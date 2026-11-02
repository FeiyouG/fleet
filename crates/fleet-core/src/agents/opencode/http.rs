//! The opencode HTTP API, available only on agents started with `--port`.

use anyhow::{Context, Result};
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

/// `POST /tui/submit-prompt` with text: types the prompt into the TUI and submits it.
pub async fn submit_prompt(port: u16, text: &str) -> Result<()> {
    let c = client();
    c.post(format!("http://127.0.0.1:{port}/tui/append-prompt"))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .context("append-prompt")?
        .error_for_status()
        .context("append-prompt status")?;
    c.post(format!("http://127.0.0.1:{port}/tui/submit-prompt"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("submit-prompt")?
        .error_for_status()
        .context("submit-prompt status")?;
    Ok(())
}
