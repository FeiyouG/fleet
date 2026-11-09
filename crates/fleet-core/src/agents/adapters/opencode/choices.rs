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

use crate::agents::{AgentChoice, Choices, CHOICES_TTL};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;

/// opencode's built-in agents that are never offered to users.
const INTERNAL_AGENTS: &[&str] = &["compaction", "summary", "title"];
const TIMEOUT: Duration = Duration::from_secs(10);

/// Cache key: the binary and the directory (a repo's `opencode.json` changes the answer).
type Key = (String, PathBuf);

fn cache() -> &'static Mutex<HashMap<Key, (Instant, Choices)>> {
    static C: OnceLock<Mutex<HashMap<Key, (Instant, Choices)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The on-disk copy, shared by every `fleet` process (each CLI call is a new process, and
/// a cold discovery takes seconds): `<data>/cache/opencode-choices.json`.
fn disk_path() -> PathBuf {
    crate::config::data_dir()
        .join("cache")
        .join("opencode-choices.json")
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    /// `"<bin>\0<cwd>"` -> (unix ms when discovered, choices)
    entries: HashMap<String, (i64, Choices)>,
}

fn disk_key(k: &Key) -> String {
    format!("{}\0{}", k.0, k.1.display())
}

fn read_disk() -> Disk {
    std::fs::read_to_string(disk_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn from_disk(k: &Key, now_ms: i64) -> Option<Choices> {
    let (at, c) = read_disk().entries.remove(&disk_key(k))?;
    (now_ms - at < CHOICES_TTL.as_millis() as i64 && now_ms >= at).then_some(c)
}

fn to_disk(k: &Key, now_ms: i64, c: &Choices) {
    let mut d = read_disk();
    d.entries
        .retain(|_, (at, _)| now_ms - *at < CHOICES_TTL.as_millis() as i64);
    d.entries.insert(disk_key(k), (now_ms, c.clone()));
    let path = disk_path();
    let tmp = path.with_extension("json.tmp");
    // best effort: a failed write only costs the next process a fresh discovery
    let _ = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::write(&tmp, serde_json::to_vec(&d).unwrap_or_default()))
        .and_then(|_| std::fs::rename(&tmp, &path));
}

/// What `bin` offers in `cwd`, cached [`CHOICES_TTL`] in memory and on disk. Failures are
/// not cached: the next call tries again.
pub async fn choices(bin: &str, cwd: &Path) -> Result<Choices> {
    let key: Key = (bin.to_string(), cwd.to_path_buf());
    if let Some((at, c)) = cache().lock().unwrap().get(&key) {
        if at.elapsed() < CHOICES_TTL {
            return Ok(c.clone());
        }
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let c = match from_disk(&key, now_ms) {
        Some(c) => c,
        None => {
            let c = discover(bin, cwd).await?;
            to_disk(&key, now_ms, &c);
            c
        }
    };
    cache()
        .lock()
        .unwrap()
        .insert(key, (Instant::now(), c.clone()));
    Ok(c)
}

async fn discover(bin: &str, cwd: &Path) -> Result<Choices> {
    let run = |args: &'static [&'static str]| run(bin, cwd, args);
    let (models, agents, config) = tokio::join!(
        run(&["models"]),
        run(&["agent", "list"]),
        run(&["debug", "config"]),
    );
    let models = parse_models(&models.context("opencode models")?);
    if models.is_empty() {
        bail!("`opencode models` listed no models (no provider credentials?)");
    }
    let names = parse_agent_list(&agents.context("opencode agent list")?);
    let details = futures::future::join_all(
        names
            .iter()
            .map(|n| async move { self::run(bin, cwd, &["debug", "agent", n]).await.ok() }),
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

async fn run(bin: &str, cwd: &Path, args: &[&str]) -> Result<String> {
    let fut = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(TIMEOUT, fut)
        .await
        .with_context(|| format!("`{bin} {}` timed out", args.join(" ")))?
        .with_context(|| format!("running `{bin} {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`{bin} {}` exited with {}: {}",
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
