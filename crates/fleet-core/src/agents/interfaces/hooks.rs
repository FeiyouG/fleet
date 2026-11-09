//! `fleet agent-hook <agent>`: the command Claude Code and Codex run on their lifecycle
//! events. Reads the event JSON on stdin and updates the agent's state file. Also the
//! idempotent merge/unmerge of those hook entries into the agents' JSON settings files.

use super::state_file::{self, Pending};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Events both Claude Code and Codex fire with the same stdin schema.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// Apply one hook event to the state file of process `pid` (the agent: the hook's parent).
pub fn apply(agent: &str, pid: u32, event: &Value) -> Result<()> {
    let name = event
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let session = event
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let cwd = event.get("cwd").and_then(Value::as_str).map(String::from);
    let mode = event
        .get("permission_mode")
        .and_then(Value::as_str)
        .map(String::from);

    if name == "SessionEnd" {
        state_file::remove(pid);
        return Ok(());
    }
    state_file::update(pid, |st| {
        st.agent = agent.to_string();
        if let Some(c) = &cwd {
            st.directory = c.into();
        }
        // One process shows one session; a new id means a resume/clear replaced the old one.
        if !st.sessions.contains_key(&session) {
            st.sessions.clear();
        }
        let s = st.session_mut(&session);
        if mode.is_some() {
            s.mode = mode.clone();
        }
        match name.as_str() {
            "SessionStart" => {
                s.status = "idle".into();
                if let Some(t) = event.get("session_title").and_then(Value::as_str) {
                    s.title = Some(t.to_string());
                }
            }
            "UserPromptSubmit" => {
                s.status = "busy".into();
                s.pending.clear();
                s.question = false;
                s.plan = false;
                if s.title.is_none() {
                    if let Some(p) = event.get("prompt").and_then(Value::as_str) {
                        let first = p
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .trim();
                        if !first.is_empty() {
                            s.title = Some(first.chars().take(80).collect());
                        }
                    }
                }
            }
            "PermissionRequest" => {
                s.status = "busy".into();
                let tool = event
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let title = format!("{tool}: {}", summarize_input(tool, event.get("tool_input")));
                if !s.pending.iter().any(|p| p.title == title) {
                    s.pending.push(Pending {
                        id: String::new(),
                        title,
                        kind: tool.to_string(),
                    });
                }
            }
            "PreToolUse" => {
                s.status = "busy".into();
                match event.get("tool_name").and_then(Value::as_str).unwrap_or("") {
                    "AskUserQuestion" | "request_user_input" => s.question = true,
                    "ExitPlanMode" => s.plan = true,
                    _ => {}
                }
            }
            "PostToolUse" | "PostToolUseFailure" => {
                s.pending.clear();
                s.question = false;
                s.plan = false;
            }
            "Notification" => {
                let kind = event
                    .get("notification_type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if kind == "permission_prompt" && s.pending.is_empty() {
                    let msg = event
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("permission needed");
                    s.pending.push(Pending {
                        id: String::new(),
                        title: msg.chars().take(120).collect(),
                        kind: "permission".into(),
                    });
                }
                if kind == "idle_prompt" {
                    s.status = "idle".into();
                }
            }
            "Stop" => {
                s.status = "idle".into();
                s.pending.clear();
                s.question = false;
                s.plan = false;
            }
            _ => {}
        }
    })
}

/// A one-line description of a tool call, for the pending list.
pub fn summarize_input(tool: &str, input: Option<&Value>) -> String {
    let Some(i) = input else { return String::new() };
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| i.get(*k).and_then(Value::as_str))
            .map(String::from)
    };
    let s = match tool {
        "Bash" | "shell" | "exec_command" | "local_shell" => {
            pick(&["command", "cmd"]).or_else(|| i.get("command").map(|c| c.to_string()))
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Read" => {
            pick(&["file_path", "path", "notebook_path"])
        }
        "WebFetch" => pick(&["url"]),
        _ => i
            .get("description")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| Some(i.to_string())),
    };
    let s = s.unwrap_or_default().replace('\n', " ");
    s.chars().take(100).collect()
}

/// Read the hook event from stdin and apply it. `pid` = the agent process (the hook's
/// parent); callers pass `std::os::unix::process::parent_id()`.
pub fn run_from_stdin(agent: &str, pid: u32) -> Result<()> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
    if text.trim().is_empty() {
        return Ok(());
    }
    let event: Value = serde_json::from_str(&text).context("hook stdin is not JSON")?;
    apply(agent, pid, &event)
}

// ───────────── settings-file merge ─────────────

fn is_ours(h: &Value, command: &str) -> bool {
    h.get("command")
        .and_then(Value::as_str)
        .map(|c| c.starts_with(command))
        .unwrap_or(false)
}

/// Add `{"type":"command","command":"<command>","async":true}` under every event in
/// `events`, inside a matcher-less group. Idempotent; other hooks are left untouched.
pub fn merge_hooks(root: &mut Value, events: &[&str], command: &str) {
    if !root.is_object() {
        *root = json!({});
    }
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    for ev in events {
        let groups = hooks
            .as_object_mut()
            .unwrap()
            .entry(*ev)
            .or_insert_with(|| json!([]));
        if !groups.is_array() {
            *groups = json!([]);
        }
        let groups = groups.as_array_mut().unwrap();
        let present = groups.iter().any(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .map(|hs| hs.iter().any(|h| is_ours(h, command)))
                .unwrap_or(false)
        });
        if !present {
            groups.push(json!({ "hooks": [ { "type": "command", "command": command, "async": true, "timeout": 10 } ] }));
        }
    }
}

/// Remove every handler whose command starts with `command`; drop groups/events left empty.
pub fn unmerge_hooks(root: &mut Value, command: &str) -> bool {
    let mut changed = false;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut empty_events = vec![];
    for (ev, groups) in hooks.iter_mut() {
        if let Some(gs) = groups.as_array_mut() {
            for g in gs.iter_mut() {
                if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = hs.len();
                    hs.retain(|h| !is_ours(h, command));
                    changed |= hs.len() != before;
                }
            }
            gs.retain(|g| {
                g.get("hooks")
                    .and_then(Value::as_array)
                    .map(|hs| !hs.is_empty())
                    .unwrap_or(true)
            });
            if gs.is_empty() {
                empty_events.push(ev.clone());
            }
        }
    }
    for ev in empty_events {
        hooks.remove(&ev);
    }
    changed
}

pub fn read_settings(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("{} is not valid JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn write_settings(path: &Path, v: &Value) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, serde_json::to_string_pretty(v)? + "\n")
        .with_context(|| format!("writing {}", path.display()))
}

/// Is every event wired to `command` in this settings file?
pub fn hooks_installed(root: &Value, events: &[&str], command: &str) -> bool {
    events.iter().all(|ev| {
        root.pointer(&format!("/hooks/{ev}"))
            .and_then(Value::as_array)
            .map(|gs| {
                gs.iter().any(|g| {
                    g.get("hooks")
                        .and_then(Value::as_array)
                        .map(|hs| hs.iter().any(|h| is_ours(h, command)))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_idempotent_and_preserves_others() {
        let mut v = json!({ "theme": "dark", "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "plannotator" } ] } ] } });
        merge_hooks(&mut v, EVENTS, "fleet agent-hook claude");
        merge_hooks(&mut v, EVENTS, "fleet agent-hook claude");
        assert!(hooks_installed(&v, EVENTS, "fleet agent-hook claude"));
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(
            stop.len(),
            2,
            "plannotator group kept, one fleet group added"
        );
        assert_eq!(v["theme"], "dark");
        assert!(unmerge_hooks(&mut v, "fleet agent-hook claude"));
        assert!(!hooks_installed(&v, EVENTS, "fleet agent-hook claude"));
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(v["hooks"].get("PermissionRequest").is_none());
    }

    #[test]
    fn event_mapping() {
        let _g = crate::agents::state_file::test_lock();
        let tmp = std::env::temp_dir().join(format!("fleet-hooks-{}", std::process::id()));
        std::env::set_var("FLEET_DATA_DIR", &tmp);
        let pid = 888_888;
        let ev = |name: &str, extra: Value| {
            let mut e = json!({ "hook_event_name": name, "session_id": "s1", "cwd": "/w", "permission_mode": "plan" });
            if let Some(o) = extra.as_object() {
                for (k, v) in o {
                    e[k] = v.clone();
                }
            }
            e
        };
        apply("claude", pid, &ev("SessionStart", json!({}))).unwrap();
        apply(
            "claude",
            pid,
            &ev(
                "UserPromptSubmit",
                json!({ "prompt": "\nFix the thing\nmore" }),
            ),
        )
        .unwrap();
        apply(
            "claude",
            pid,
            &ev(
                "PermissionRequest",
                json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf dist" } }),
            ),
        )
        .unwrap();
        let st = state_file::parse(&std::fs::read_to_string(state_file::path_for(pid)).unwrap())
            .unwrap();
        let s = &st.sessions["s1"];
        assert_eq!(st.agent, "claude");
        assert_eq!(s.status, "busy");
        assert_eq!(s.title.as_deref(), Some("Fix the thing"));
        assert_eq!(s.mode.as_deref(), Some("plan"));
        assert_eq!(s.pending[0].title, "Bash: rm -rf dist");
        apply(
            "claude",
            pid,
            &ev("PostToolUse", json!({ "tool_name": "Bash" })),
        )
        .unwrap();
        apply(
            "claude",
            pid,
            &ev("PreToolUse", json!({ "tool_name": "AskUserQuestion" })),
        )
        .unwrap();
        let st = state_file::parse(&std::fs::read_to_string(state_file::path_for(pid)).unwrap())
            .unwrap();
        assert!(st.sessions["s1"].pending.is_empty() && st.sessions["s1"].question);
        apply("claude", pid, &ev("Stop", json!({}))).unwrap();
        let st = state_file::parse(&std::fs::read_to_string(state_file::path_for(pid)).unwrap())
            .unwrap();
        assert_eq!(st.sessions["s1"].status, "idle");
        apply("claude", pid, &ev("SessionEnd", json!({}))).unwrap();
        assert!(!state_file::path_for(pid).exists());
        std::fs::remove_dir_all(&tmp).ok();
        std::env::remove_var("FLEET_DATA_DIR");
    }
}
