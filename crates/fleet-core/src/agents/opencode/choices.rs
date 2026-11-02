//! What an opencode install offers from a given directory: its primary agents (with their
//! own default model, if any), every model it has credentials for, and the global default.
//!
//! Sources, all run with the project repo as cwd (a repo's `opencode.json` can add
//! providers and agents):
//! - `opencode models`            -> one `provider/model` per line
//! - `opencode agent list`        -> `name (primary|subagent|all)` header lines
//! - `opencode debug agent NAME`  -> JSON with `hidden` and `model`
//! - `opencode debug config`      -> JSON with the global `model`
//!
//! `debug` is a debugging command whose output may change; when it fails we still list the
//! agents, dropping opencode's known internal ones by name.

use crate::agents::{AgentChoice, Choices};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;

/// opencode's built-in agents that are never offered to users.
const INTERNAL_AGENTS: &[&str] = &["compaction", "summary", "title"];
const TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_TTL: Duration = Duration::from_secs(300);

fn cache() -> &'static Mutex<HashMap<PathBuf, (Instant, Choices)>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, (Instant, Choices)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub async fn choices(cwd: &Path) -> Result<Choices> {
    if let Some((at, c)) = cache().lock().unwrap().get(cwd) {
        if at.elapsed() < CACHE_TTL {
            return Ok(c.clone());
        }
    }
    let c = discover(cwd).await?;
    cache()
        .lock()
        .unwrap()
        .insert(cwd.to_path_buf(), (Instant::now(), c.clone()));
    Ok(c)
}

async fn discover(cwd: &Path) -> Result<Choices> {
    let (models, agents, config) = tokio::join!(
        run(cwd, &["models"]),
        run(cwd, &["agent", "list"]),
        run(cwd, &["debug", "config"]),
    );
    let models = parse_models(&models.context("opencode models")?);
    if models.is_empty() {
        bail!("`opencode models` listed no models (no provider credentials?)");
    }
    let names = parse_agent_list(&agents.context("opencode agent list")?);
    let details = futures::future::join_all(
        names
            .iter()
            .map(|n| async move { run(cwd, &["debug", "agent", n]).await.ok() }),
    )
    .await;
    let agents = names
        .into_iter()
        .zip(details)
        .filter_map(|(name, d)| agent_choice(name, d.as_deref()))
        .collect();
    let default_model = config.ok().and_then(|c| json_str(&c, "model"));
    Ok(Choices {
        agents,
        models,
        default_model,
    })
}

async fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    let fut = Command::new("opencode")
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(TIMEOUT, fut)
        .await
        .with_context(|| format!("`opencode {}` timed out", args.join(" ")))?
        .with_context(|| format!("running `opencode {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`opencode {}` exited with {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Lines shaped `provider/model` (anything else, e.g. a stray log line, is skipped).
pub fn parse_models(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.contains(char::is_whitespace)
                && l.split_once('/')
                    .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
        })
        .map(String::from)
        .collect()
}

/// Names of agents usable as the main agent: header lines `name (primary)` or `name (all)`.
/// The permission JSON printed under each header is ignored.
pub fn parse_agent_list(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| !l.starts_with(char::is_whitespace))
        .filter_map(|l| {
            let (name, kind) = l.trim_end().split_once(' ')?;
            matches!(kind, "(primary)" | "(all)").then(|| name.to_string())
        })
        .collect()
}

/// Combine an agent name with its `debug agent` JSON. `None` = hidden (internal) agent.
pub fn agent_choice(name: String, detail: Option<&str>) -> Option<AgentChoice> {
    let v: Option<Value> = detail.and_then(|d| serde_json::from_str(json_body(d)).ok());
    match v {
        Some(v) => {
            if v.get("hidden").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            Some(AgentChoice {
                model: model_id(v.get("model")),
                name,
            })
        }
        None if INTERNAL_AGENTS.contains(&name.as_str()) => None,
        None => Some(AgentChoice { name, model: None }),
    }
}

/// An agent's `model` is either `"provider/model"` or `{providerID, modelID}`.
fn model_id(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(o) => Some(format!(
            "{}/{}",
            o.get("providerID")?.as_str()?,
            o.get("modelID")?.as_str()?
        )),
        _ => None,
    }
}

fn json_str(out: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json_body(out)).ok()?;
    v.get(key)?.as_str().map(String::from)
}

/// Skip anything printed before the JSON object.
fn json_body(s: &str) -> &str {
    s.find('{').map(|i| &s[i..]).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        let out = "INFO  2026-11-02 loading providers\nanthropic/claude-opus-5-5\nopenai/gpt-5.4\n\nopencode/big-pickle\nnot a model\n";
        assert_eq!(
            parse_models(out),
            vec![
                "anthropic/claude-opus-5-5",
                "openai/gpt-5.4",
                "opencode/big-pickle"
            ]
        );
    }

    #[test]
    fn agent_list() {
        let out = "build (primary)\n  [\n  {\n    \"permission\": \"*\",\n    \"action\": \"allow\"\n  }\n]\ncompaction (primary)\n  []\nexplore (subagent)\n  []\nreviewer (all)\n  []\nplan (primary)\n  []\n";
        assert_eq!(
            parse_agent_list(out),
            vec!["build", "compaction", "reviewer", "plan"]
        );
    }

    #[test]
    fn agent_details_and_fallback() {
        let plan = agent_choice(
            "plan".into(),
            Some(r#"{"name":"plan","mode":"primary","model":{"providerID":"openai","modelID":"gpt-5.4"}}"#),
        )
        .unwrap();
        assert_eq!(plan.model.as_deref(), Some("openai/gpt-5.4"));
        assert!(agent_choice("title".into(), Some(r#"{"name":"title","hidden":true}"#)).is_none());
        assert_eq!(
            agent_choice("build".into(), Some(r#"{"name":"build","model":null}"#))
                .unwrap()
                .model,
            None
        );
        // `debug agent` failed: internal names dropped, others kept
        assert!(agent_choice("compaction".into(), None).is_none());
        assert_eq!(agent_choice("custom".into(), None).unwrap().name, "custom");
        assert_eq!(
            json_str("log line\n{\"model\":\"anthropic/x\"}", "model").as_deref(),
            Some("anthropic/x")
        );
    }
}
