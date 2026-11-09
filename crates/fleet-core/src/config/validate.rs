//! Config validation. Two classes:
//!
//! - **fatal**: fleet cannot run (no agents, the default agent is not defined, an unknown
//!   harness, two tasks on one key, …). The TUI shows them in a blocking popup and quits;
//!   the CLI exits 1.
//! - **warnings**: fleet runs, the affected item is skipped or greyed (binary not on PATH, a
//!   skill or repo path that does not exist, an unknown key — probably a typo).

use super::{Config, On};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Issues {
    pub fatal: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn check(cfg: &Config) -> Issues {
    let mut i = Issues::default();
    let known = crate::agents::KNOWN;

    if cfg.agents.is_empty() {
        i.fatal.push(format!(
            "`agents` is empty: define at least one, e.g. agents = {{ opencode = {{ harness = \"opencode\" }} }} (harnesses: {})",
            known.join(", ")
        ));
    }
    for (name, a) in &cfg.agents {
        match &a.harness {
            None => i.fatal.push(format!(
                "agents.{name}: `harness` is required (one of {}); only an agent named after a harness may omit it",
                known.join(", ")
            )),
            Some(h) if !known.contains(&h.as_str()) => i.fatal.push(format!(
                "agents.{name}: unknown harness \"{h}\" (known: {})",
                known.join(", ")
            )),
            _ => {}
        }
        if a.sandbox.is_some() {
            i.warnings.push(format!(
                "agents.{name}.sandbox is no longer used: modes (plan | build | yolo) decide the harness's sandbox; remove it"
            ));
        }
    }
    for p in &cfg.projects {
        for (name, a) in &p.agents {
            if let Some(h) = &a.harness {
                if !known.contains(&h.as_str()) {
                    i.fatal.push(format!(
                        "projects.{}.agents.{name}: unknown harness \"{h}\"",
                        p.name
                    ));
                }
            }
            if cfg.agent(name, Some(p)).is_none() {
                i.fatal.push(format!(
                    "projects.{}.agents.{name}: not a global agent and no `harness` given",
                    p.name
                ));
            }
        }
    }

    // defaults.agent (global and per project) must resolve to an agent.
    let check_default_agent = |where_: &str, d: &super::Defaults, project| -> Option<String> {
        match cfg.agent(&d.agent, project) {
            Some(_) => None,
            None => Some(format!(
                "{where_}: agent \"{}\" is not defined (agents: {})",
                d.agent,
                cfg.agent_names(project).join(", ")
            )),
        }
    };
    if let Some(e) = check_default_agent("defaults.agent", &cfg.defaults, None) {
        i.fatal.push(e);
    }
    if crate::agents::Mode::parse(&cfg.defaults.mode).is_none() {
        i.fatal
            .push("defaults.mode is empty; use plan, build, yolo or a harness mode name".into());
    }
    for p in &cfg.projects {
        if let Some(e) = check_default_agent(
            &format!("projects.{}.defaults.agent", p.name),
            &cfg.defaults_for(Some(p)),
            Some(p),
        ) {
            i.fatal.push(e);
        }
    }

    // projects: unique names, existing repos.
    let mut seen = BTreeMap::new();
    for p in &cfg.projects {
        if seen.insert(p.name.clone(), ()).is_some() {
            i.fatal
                .push(format!("projects: name \"{}\" is used twice", p.name));
        }
        if !p.repo.exists() {
            i.warnings.push(format!(
                "projects.{}: repo {} does not exist",
                p.name,
                p.repo.display()
            ));
        }
    }

    // tasks: resolvable, valid `on`, unique keys per project scope.
    let scopes: Vec<Option<&super::Project>> = std::iter::once(None)
        .chain(cfg.projects.iter().map(Some))
        .collect();
    for scope in scopes {
        let where_ = match scope {
            None => String::new(),
            Some(p) => format!(" (project {})", p.name),
        };
        let mut by_key: BTreeMap<String, String> = BTreeMap::new();
        for (name, r) in cfg.tasks_for(scope) {
            match r {
                Err(e) => i.fatal.push(format!("tasks.{name}{where_}: {e:#}")),
                Ok(t) => {
                    debug_assert!(matches!(t.on, On::Project | On::Review));
                    if let Some(k) = &t.key {
                        if t.enabled {
                            if let Some(other) = by_key.insert(k.clone(), name.clone()) {
                                i.fatal.push(format!(
                                    "tasks{where_}: `{other}` and `{name}` both use key \"{k}\""
                                ));
                            }
                        }
                    }
                    for s in &t.skills {
                        if let Some(w) = skill_problem(s) {
                            i.warnings.push(format!("tasks.{name}{where_}: {w}"));
                        }
                    }
                }
            }
        }
    }

    // agents: binaries on PATH (a missing one greys the agent out, it does not stop fleet).
    for a in cfg.agents_for(None) {
        if a.enabled && !on_path(&a.bin) {
            i.warnings.push(format!(
                "agents.{}: `{}` not found on PATH — install {} or set bin",
                a.name, a.bin, a.harness
            ));
        }
    }

    i.fatal.sort();
    i.fatal.dedup();
    i.warnings.sort();
    i.warnings.dedup();
    i
}

/// A skill must be an existing `SKILL.md` with a `name:` front-matter line (what every harness
/// uses to register it).
pub fn skill_problem(path: &Path) -> Option<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Some(format!("skill {} does not exist", path.display()));
    };
    let has_name = text
        .lines()
        .take(20)
        .any(|l| l.trim_start().starts_with("name:"));
    (!has_name).then(|| {
        format!(
            "skill {} has no `name:` front-matter; agents will not find it",
            path.display()
        )
    })
}

/// Name of the skill from its front-matter, else the folder name.
pub fn skill_name(path: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(path) {
        for l in text.lines().take(20) {
            if let Some(n) = l.trim_start().strip_prefix("name:") {
                return n.trim().trim_matches('"').to_string();
            }
        }
    }
    path.parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn on_path(bin: &str) -> bool {
    if bin.contains('/') {
        return Path::new(bin).exists();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

// ───────────────────────── unknown keys ─────────────────────────

const TOP: &[&str] = &[
    "defaults",
    "agents",
    "tasks",
    "keys",
    "projects",
    "agent_args",
];
const DEFAULTS: &[&str] = &[
    "agent",
    "mode",
    "left_pane",
    "nvim_pane",
    "notify",
    "refresh_secs",
    "review_ttl_secs",
    "system",
];
const AGENT: &[&str] = &[
    "harness", "enabled", "bin", "args", "model", "skills", "system", "sandbox",
];
const TASK: &[&str] = &[
    "key",
    "on",
    "enabled",
    "agent",
    "mode",
    "model",
    "skills",
    "instructions",
    "prompt",
    "label",
];
const PROJECT: &[&str] = &[
    "name",
    "repo",
    "worktrees",
    "reviews",
    "base",
    "defaults",
    "agents",
    "tasks",
    "keys",
];

/// Keys the schema does not know, each with the closest known one — a typo would otherwise
/// be silently ignored.
pub fn unknown_keys(v: &Value) -> Vec<String> {
    let mut out = vec![];
    let obj = |v: &Value, path: &str, allowed: &[&str], out: &mut Vec<String>| {
        if let Value::Object(m) = v {
            for k in m.keys() {
                if !allowed.contains(&k.as_str()) {
                    out.push(match closest(k, allowed) {
                        Some(c) => format!("{path}{k}: unknown key (did you mean `{c}`?)"),
                        None => format!("{path}{k}: unknown key"),
                    });
                }
            }
        }
    };
    obj(v, "", TOP, &mut out);
    obj(&v["defaults"], "defaults.", DEFAULTS, &mut out);
    if let Value::Object(agents) = &v["agents"] {
        for (n, a) in agents {
            obj(a, &format!("agents.{n}."), AGENT, &mut out);
        }
    }
    if let Value::Object(tasks) = &v["tasks"] {
        for (n, t) in tasks {
            obj(t, &format!("tasks.{n}."), TASK, &mut out);
        }
    }
    if let Value::Array(projects) = &v["projects"] {
        for p in projects {
            let name = p["name"].as_str().unwrap_or("?");
            let pre = format!("projects.{name}.");
            obj(p, &pre, PROJECT, &mut out);
            obj(
                &p["defaults"],
                &format!("{pre}defaults."),
                DEFAULTS,
                &mut out,
            );
            if let Value::Object(agents) = &p["agents"] {
                for (n, a) in agents {
                    obj(a, &format!("{pre}agents.{n}."), AGENT, &mut out);
                }
            }
            if let Value::Object(tasks) = &p["tasks"] {
                for (n, t) in tasks {
                    obj(t, &format!("{pre}tasks.{n}."), TASK, &mut out);
                }
            }
        }
    }
    out
}

fn closest<'a>(k: &str, allowed: &[&'a str]) -> Option<&'a str> {
    allowed
        .iter()
        .map(|a| (edit_distance(k, a), *a))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, a)| a)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_keys_with_suggestions() {
        let v = json!({"defualts": {}, "defaults": {"moed": 1}, "tasks": {"x": {"skilz": []}},
                       "projects": [{"name": "p", "repo": "/r", "revews": "x"}]});
        let w = unknown_keys(&v);
        assert!(w
            .iter()
            .any(|s| s.contains("defualts") && s.contains("`defaults`")));
        assert!(w
            .iter()
            .any(|s| s.contains("defaults.moed") && s.contains("`mode`")));
        assert!(w
            .iter()
            .any(|s| s.contains("tasks.x.skilz") && s.contains("`skills`")));
        assert!(w
            .iter()
            .any(|s| s.contains("projects.p.revews") && s.contains("`reviews`")));
    }

    #[test]
    fn skill_front_matter() {
        let d = std::env::temp_dir().join(format!("fleet-skill-{}", std::process::id()));
        std::fs::create_dir_all(d.join("good")).unwrap();
        std::fs::write(d.join("good/SKILL.md"), "---\nname: good-skill\n---\n# x").unwrap();
        std::fs::write(d.join("bad.md"), "# no front matter").unwrap();
        assert_eq!(skill_problem(&d.join("good/SKILL.md")), None);
        assert_eq!(skill_name(&d.join("good/SKILL.md")), "good-skill");
        assert!(skill_problem(&d.join("bad.md")).unwrap().contains("name:"));
        assert!(skill_problem(&d.join("none.md"))
            .unwrap()
            .contains("does not exist"));
        std::fs::remove_dir_all(d).ok();
    }
}
