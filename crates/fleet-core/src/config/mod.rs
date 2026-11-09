//! Configuration: the shipped defaults (`config/default/` at the repo root, embedded) with
//! the user's `~/.config/fleet/init.lua` deep-merged over them.
//!
//! Three layers, each one above the next:
//! - a **harness** is an adapter implementation (`opencode`, `claude`, `codex`);
//! - an **agent** is the user's named configuration of a harness (`agents.<name>`);
//! - a **task** is a preset for starting an agent (`tasks.<name>`): on what (`project` →
//!   new worktree, `review` → MR/PR head), with which agent, mode, model, skills,
//!   instructions, prompt and key.
//!
//! Every global section (`defaults`, `agents`, `tasks`, `keys`) can be overridden per
//! project, field by field. Resolution order for any field: CLI/popup → project task →
//! global task → project agent → global agent → project defaults → global defaults, with the
//! user's file over the shipped one at each level. Rust reads only the merged result and
//! assumes every field is present; nothing is hard-coded here.

pub mod lua;
pub mod merge;
pub mod shipped;
pub mod validate;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub use validate::Issues;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub defaults: Defaults,
    /// Agents by the user's name for them.
    pub agents: BTreeMap<String, AgentDef>,
    /// Tasks by name.
    pub tasks: BTreeMap<String, TaskDef>,
    /// action -> key (see `config/default/keys/mod.lua`).
    pub keys: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "lua_project_list")]
    pub projects: Vec<Project>,
    /// (legacy) extra arguments for opencode; prefer `agents.opencode.args`.
    #[serde(default, deserialize_with = "lua_string_list")]
    pub agent_args: Vec<String>,
    /// Non-fatal problems found while loading (shown once by the TUI, printed by the CLI).
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    /// The agent new tasks use unless the task or project says otherwise. Not shipped: the
    /// user's file must set it (a missing or unknown value is a warning until a task starts).
    #[serde(default)]
    pub agent: String,
    /// `plan` | `build` | `yolo`, or a harness-specific mode (an opencode agent name).
    pub mode: String,
    /// Command typed into the LEFT (shell) pane of a new task; `None` = bare shell.
    /// (Legacy `nvim_pane = true|false` still parses: `true` means `"nvim"`.)
    #[serde(default, deserialize_with = "left_pane_compat", alias = "nvim_pane")]
    pub left_pane: Option<String>,
    pub notify: bool,
    pub refresh_secs: u64,
    pub review_ttl_secs: u64,
    /// fleet's system text, sent first to every agent. Placeholders: `{slug} {repo}
    /// {branch} {base} {agent} {review_id} {review_url} {review_title} {source_branch}
    /// {target_branch}`.
    pub system: String,
}

/// `defaults` as a project may override it: every field optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsPatch {
    pub agent: Option<String>,
    pub mode: Option<String>,
    #[serde(deserialize_with = "left_pane_compat", alias = "nvim_pane")]
    pub left_pane: Option<String>,
    pub notify: Option<bool>,
    pub refresh_secs: Option<u64>,
    pub review_ttl_secs: Option<u64>,
    pub system: Option<String>,
}

/// An agent as written in config (global or as a project override): every field optional
/// so a project can change one thing. Resolved by [`Config::agent`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentDef {
    /// `opencode` | `claude` | `codex`. Required on a global agent (a global agent whose
    /// key is a harness name may omit it).
    pub harness: Option<String>,
    pub enabled: Option<bool>,
    /// Executable name or path. Defaults to the harness name.
    pub bin: Option<String>,
    /// Extra arguments appended to every launch.
    #[serde(default, deserialize_with = "lua_string_list_opt")]
    pub args: Option<Vec<String>>,
    /// Model in the harness's own naming (opencode `provider/model`, claude `opus`, codex
    /// `gpt-5.4`). `None` = the harness's own default.
    pub model: Option<String>,
    /// Absolute paths to `SKILL.md` files always loaded for this agent.
    #[serde(default, deserialize_with = "lua_path_list_opt")]
    pub skills: Option<Vec<PathBuf>>,
    /// Text appended to fleet's system text for this agent.
    pub system: Option<String>,
    /// (legacy) codex `--sandbox` for build mode; modes are adapter code now. Warned about.
    pub sandbox: Option<String>,
}

/// A task as written in config (global or project override).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskDef {
    /// The key after the tasks prefix (`Space t`), e.g. `"s"`. `None` = menu only.
    pub key: Option<String>,
    /// `project` (new worktree from base) | `review` (MR/PR head). Defaults to `project`.
    pub on: Option<String>,
    pub enabled: Option<bool>,
    pub agent: Option<String>,
    pub mode: Option<String>,
    pub model: Option<String>,
    #[serde(default, deserialize_with = "lua_path_list_opt")]
    pub skills: Option<Vec<PathBuf>>,
    /// Text after the system text, before the prompt.
    pub instructions: Option<String>,
    /// Preset user prompt (editable in the popup).
    pub prompt: Option<String>,
    /// Menu label; defaults to the name.
    pub label: Option<String>,
}

/// One repo you work in, with optional overrides of every global section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// The main clone.
    pub repo: PathBuf,
    /// Where new worktrees go. Defaults to `~/.local/share/fleet/worktrees/<name>`.
    #[serde(default)]
    pub worktrees: Option<PathBuf>,
    /// `gitlab:group/project` or `github:owner/repo`. `None` = no Reviews.
    #[serde(default)]
    pub reviews: Option<String>,
    /// Base branch for new work. Defaults to `main`.
    #[serde(default = "default_base")]
    pub base: String,
    #[serde(default)]
    pub defaults: DefaultsPatch,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentDef>,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskDef>,
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

fn default_base() -> String {
    "main".into()
}

impl Project {
    /// A project with only the required fields (tests, ad-hoc repos outside the config).
    pub fn new(name: impl Into<String>, repo: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            repo: repo.into(),
            worktrees: None,
            reviews: None,
            base: "main".into(),
            defaults: DefaultsPatch::default(),
            agents: BTreeMap::new(),
            tasks: BTreeMap::new(),
            keys: BTreeMap::new(),
        }
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        self.worktrees
            .clone()
            .unwrap_or_else(|| data_dir().join("worktrees").join(&self.name))
    }
}

/// What a task is started on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum On {
    Project,
    Review,
}

impl On {
    pub fn parse(s: &str) -> Option<On> {
        match s {
            "project" => Some(On::Project),
            "review" => Some(On::Review),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            On::Project => "project",
            On::Review => "review",
        }
    }
}

/// An agent with every layer applied. What adapters and launches consume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedAgent {
    pub name: String,
    pub harness: String,
    pub enabled: bool,
    pub bin: String,
    pub args: Vec<String>,
    pub model: Option<String>,
    pub skills: Vec<PathBuf>,
    pub system: Option<String>,
}

/// What an adapter is constructed with: the harness-level view of one resolved agent.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    pub enabled: bool,
    pub bin: Option<String>,
    pub args: Vec<String>,
    pub model: Option<String>,
}

impl From<&ResolvedAgent> for AgentConfig {
    fn from(a: &ResolvedAgent) -> Self {
        Self {
            enabled: a.enabled,
            bin: Some(a.bin.clone()),
            args: a.args.clone(),
            model: a.model.clone(),
        }
    }
}

/// A task with every layer applied: everything `fleet task <name>` needs.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedTask {
    pub name: String,
    pub label: String,
    pub on: On,
    pub key: Option<String>,
    pub enabled: bool,
    pub agent: ResolvedAgent,
    pub mode: String,
    pub model: Option<String>,
    /// Agent skills then task skills, deduplicated, in order.
    pub skills: Vec<PathBuf>,
    /// fleet's system text (project/global `defaults.system`), with the agent's `system`
    /// appended.
    pub system: String,
    pub instructions: Option<String>,
    pub prompt: Option<String>,
}

// ───────────────────────── deserialize helpers ─────────────────────────

/// `left_pane = "nvim"` | `left_pane = ""` (bare shell) | legacy `nvim_pane = true|false`.
fn left_pane_compat<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Flag(bool),
        Cmd(String),
    }
    Ok(match Option::<Raw>::deserialize(d)? {
        None | Some(Raw::Flag(false)) => None,
        Some(Raw::Flag(true)) => Some("nvim".into()),
        Some(Raw::Cmd(c)) => Some(c.trim().to_string()).filter(|c| !c.is_empty()),
    })
}

/// A Lua `{}` is an empty *table*, which arrives as a map, not a sequence. Accept both.
fn lua_string_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Seq(Vec<String>),
        Map(BTreeMap<String, String>),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::Seq(v) => v,
        Raw::Map(m) => m.into_values().collect(),
    })
}

/// `projects = {}` (empty Lua table) arrives as a map; a list of tables as a sequence.
fn lua_project_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Project>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Seq(Vec<Project>),
        Map(BTreeMap<String, Project>),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::Seq(v) => v,
        Raw::Map(m) => m.into_values().collect(),
    })
}

fn lua_string_list_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Seq(Vec<String>),
        Map(BTreeMap<String, String>),
    }
    Ok(Option::<Raw>::deserialize(d)?.map(|r| match r {
        Raw::Seq(v) => v,
        Raw::Map(m) => m.into_values().collect(),
    }))
}

fn lua_path_list_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<PathBuf>>, D::Error> {
    Ok(lua_string_list_opt(d)?.map(|v| v.into_iter().map(|s| expand(Path::new(&s))).collect()))
}

// ───────────────────────── paths ─────────────────────────

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn xdg_config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

/// `$FLEET_CONFIG_DIR`, else `$XDG_CONFIG_HOME/fleet` or `~/.config/fleet`.
pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    xdg_config_home().join("fleet")
}

pub fn config_path() -> PathBuf {
    config_dir().join("init.lua")
}

/// `$FLEET_DATA_DIR`, else `$XDG_DATA_HOME/fleet` or `~/.local/share/fleet`.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_DATA_DIR") {
        return PathBuf::from(p);
    }
    xdg_data_home().join("fleet")
}

pub fn xdg_data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/share"))
}

pub fn home() -> PathBuf {
    dirs::home_dir().expect("HOME")
}

/// `~/x` -> `$HOME/x`.
pub fn expand(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        home().join(rest)
    } else if s == "~" {
        home()
    } else {
        p.to_path_buf()
    }
}

// ───────────────────────── loading ─────────────────────────

impl Config {
    /// The shipped defaults with the user's `~/.config/fleet/init.lua` merged over them.
    /// Fatal problems are an `Err` listing every one; warnings are kept on `.warnings`.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    /// Like `load`, with the user entry file given (its folder is the user tree).
    pub fn load_from(path: &Path) -> Result<Self> {
        let user = if path.exists() {
            let dir = path.parent().unwrap_or(Path::new("."));
            let entry = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "init.lua".into());
            Some((lua::Tree::Dir(dir.to_path_buf()), entry))
        } else {
            None
        };
        Self::load_layers(user.as_ref().map(|(t, e)| (t, e.as_str())))
    }

    /// The shipped defaults alone (no user file). What a fresh machine runs with.
    pub fn shipped() -> Result<Self> {
        Self::load_layers(None)
    }

    /// Test / `--check` entry: `src` is the user layer, evaluated over the shipped defaults.
    pub fn from_lua(src: &str, chunk_name: &str) -> Result<Self> {
        let tree = lua::memory(chunk_name, src);
        Self::load_layers(Some((&tree, chunk_name)))
    }

    /// The merged tree as JSON (what `--effective` prints), before typing.
    pub fn effective_json(user: Option<(&lua::Tree, &str)>) -> Result<serde_json::Value> {
        let base =
            lua::eval(&lua::Tree::Embedded(shipped::FILES), "mod.lua").context("shipped config")?;
        let merged = match user {
            Some((tree, entry)) => merge::deep_merge(base, lua::eval(tree, entry)?),
            None => base,
        };
        Ok(merged)
    }

    fn load_layers(user: Option<(&lua::Tree, &str)>) -> Result<Self> {
        let merged = Self::effective_json(user)?;
        let mut warnings = validate::unknown_keys(&merged);
        let mut cfg: Config = serde_json::from_value(merged).map_err(|e| {
            anyhow::anyhow!(
                "{}: {e}",
                user.map(|(t, e)| format!("{}/{e}", t.describe()))
                    .unwrap_or_else(|| "<shipped>".into())
            )
        })?;
        cfg.normalise();
        let issues = cfg.validate();
        if !issues.fatal.is_empty() {
            bail!("{}", issues.fatal.join("\n"));
        }
        warnings.extend(issues.warnings);
        cfg.warnings = warnings;
        Ok(cfg)
    }

    fn normalise(&mut self) {
        for p in &mut self.projects {
            p.repo = expand(&p.repo);
            if let Some(w) = &p.worktrees {
                p.worktrees = Some(expand(w));
            }
        }
        // A global agent named after a harness may omit `harness`.
        for (name, a) in &mut self.agents {
            if a.harness.is_none() && crate::agents::KNOWN.contains(&name.as_str()) {
                a.harness = Some(name.clone());
            }
        }
        // Legacy `agent_args`: extra opencode args when the agent sets none.
        if !self.agent_args.is_empty() {
            if let Some(oc) = self.agents.get_mut("opencode") {
                if oc.args.as_deref().map(|a| a.is_empty()).unwrap_or(true) {
                    oc.args = Some(self.agent_args.clone());
                }
            }
        }
    }

    /// Fatal problems and warnings. Fatal = fleet cannot run.
    pub fn validate(&self) -> Issues {
        validate::check(self)
    }

    // ───────── lookups ─────────

    pub fn project(&self, name: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name == name)
    }

    /// The configured project whose repo contains `path` (by canonical prefix), if any.
    pub fn project_for_path(&self, path: &Path) -> Option<&Project> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.projects.iter().find(|p| {
            let rc = p.repo.canonicalize().unwrap_or_else(|_| p.repo.clone());
            canon.starts_with(&rc)
        })
    }

    // ───────── resolution ─────────

    /// `defaults` with a project's overrides applied.
    pub fn defaults_for(&self, project: Option<&Project>) -> Defaults {
        let mut d = self.defaults.clone();
        if let Some(p) = project {
            let o = &p.defaults;
            if let Some(v) = &o.agent {
                d.agent = v.clone();
            }
            if let Some(v) = &o.mode {
                d.mode = v.clone();
            }
            if o.left_pane.is_some() {
                d.left_pane = o.left_pane.clone();
            }
            if let Some(v) = o.notify {
                d.notify = v;
            }
            if let Some(v) = o.refresh_secs {
                d.refresh_secs = v;
            }
            if let Some(v) = o.review_ttl_secs {
                d.review_ttl_secs = v;
            }
            if let Some(v) = &o.system {
                d.system = v.clone();
            }
        }
        d
    }

    /// `keys` with a project's overrides applied.
    pub fn keys_for(&self, project: Option<&Project>) -> BTreeMap<String, String> {
        let mut k = self.keys.clone();
        if let Some(p) = project {
            for (a, v) in &p.keys {
                k.insert(a.clone(), v.clone());
            }
        }
        k
    }

    /// Names of every agent known globally or in the project.
    pub fn agent_names(&self, project: Option<&Project>) -> Vec<String> {
        let mut names: BTreeSet<String> = self.agents.keys().cloned().collect();
        if let Some(p) = project {
            names.extend(p.agents.keys().cloned());
        }
        names.into_iter().collect()
    }

    /// One agent, project override over global definition. `None` when no layer defines it
    /// or no layer names a harness.
    pub fn agent(&self, name: &str, project: Option<&Project>) -> Option<ResolvedAgent> {
        let g = self.agents.get(name);
        let p = project.and_then(|p| p.agents.get(name));
        if g.is_none() && p.is_none() {
            return None;
        }
        let pick = |f: fn(&AgentDef) -> Option<String>| p.and_then(f).or_else(|| g.and_then(f));
        let harness = pick(|a| a.harness.clone())?;
        let bin = pick(|a| a.bin.clone()).unwrap_or_else(|| harness.clone());
        let args = p
            .and_then(|a| a.args.clone())
            .or_else(|| g.and_then(|a| a.args.clone()))
            .unwrap_or_default();
        let skills = p
            .and_then(|a| a.skills.clone())
            .or_else(|| g.and_then(|a| a.skills.clone()))
            .unwrap_or_default();
        Some(ResolvedAgent {
            name: name.to_string(),
            harness,
            enabled: p
                .and_then(|a| a.enabled)
                .or_else(|| g.and_then(|a| a.enabled))
                .unwrap_or(true),
            bin,
            args,
            model: pick(|a| a.model.clone()),
            skills,
            system: pick(|a| a.system.clone()),
        })
    }

    /// Every resolvable agent for the project, enabled ones first, by name.
    pub fn agents_for(&self, project: Option<&Project>) -> Vec<ResolvedAgent> {
        self.agent_names(project)
            .iter()
            .filter_map(|n| self.agent(n, project))
            .collect()
    }

    /// Names of the enabled agents (global layer), for adapter construction.
    pub fn enabled_agents(&self) -> Vec<ResolvedAgent> {
        self.agents_for(None)
            .into_iter()
            .filter(|a| a.enabled)
            .collect()
    }

    /// Names of every task known globally or in the project.
    pub fn task_names(&self, project: Option<&Project>) -> Vec<String> {
        let mut names: BTreeSet<String> = self.tasks.keys().cloned().collect();
        if let Some(p) = project {
            names.extend(p.tasks.keys().cloned());
        }
        names.into_iter().collect()
    }

    /// One task with every layer applied. `Err` names what is missing (unknown task, agent
    /// not defined, invalid `on`).
    pub fn task(&self, name: &str, project: Option<&Project>) -> Result<ResolvedTask> {
        let g = self.tasks.get(name);
        let p = project.and_then(|p| p.tasks.get(name));
        if g.is_none() && p.is_none() {
            bail!(
                "no task `{name}` (known: {})",
                self.task_names(project).join(", ")
            );
        }
        let pick = |f: fn(&TaskDef) -> Option<String>| p.and_then(f).or_else(|| g.and_then(f));
        let d = self.defaults_for(project);
        let on_s = pick(|t| t.on.clone()).unwrap_or_else(|| "project".into());
        let on = On::parse(&on_s)
            .with_context(|| format!("task `{name}`: on = \"{on_s}\" (use project or review)"))?;
        let agent_name = pick(|t| t.agent.clone()).unwrap_or_else(|| d.agent.clone());
        if agent_name.is_empty() {
            bail!(
                "no agent configured: add to ~/.config/fleet/init.lua\n  agents = {{ opencode = {{ harness = \"opencode\" }} }},\n  defaults = {{ agent = \"opencode\" }},\n(see README › Getting started)"
            );
        }
        let agent = self.agent(&agent_name, project).with_context(|| {
            format!(
                "task `{name}`: agent `{agent_name}` is not defined (agents: {})",
                if self.agents.is_empty() {
                    "none".to_string()
                } else {
                    self.agent_names(project).join(", ")
                }
            )
        })?;
        let mut skills = agent.skills.clone();
        for s in p
            .and_then(|t| t.skills.clone())
            .or_else(|| g.and_then(|t| t.skills.clone()))
            .unwrap_or_default()
        {
            if !skills.contains(&s) {
                skills.push(s);
            }
        }
        let mut system = d.system.clone();
        if let Some(extra) = &agent.system {
            system.push('\n');
            system.push_str(extra);
        }
        Ok(ResolvedTask {
            name: name.to_string(),
            label: pick(|t| t.label.clone()).unwrap_or_else(|| name.to_string()),
            on,
            key: pick(|t| t.key.clone()).filter(|k| !k.is_empty()),
            enabled: p
                .and_then(|t| t.enabled)
                .or_else(|| g.and_then(|t| t.enabled))
                .unwrap_or(true),
            mode: pick(|t| t.mode.clone()).unwrap_or_else(|| d.mode.clone()),
            model: pick(|t| t.model.clone()).or_else(|| agent.model.clone()),
            skills,
            system,
            instructions: pick(|t| t.instructions.clone()),
            prompt: pick(|t| t.prompt.clone()),
            agent,
        })
    }

    /// A task's key and label without resolving its agent (menus list tasks even when the
    /// agent is missing, greyed with the reason).
    pub fn task_key_label(
        &self,
        name: &str,
        project: Option<&Project>,
    ) -> Option<(Option<String>, String, bool)> {
        let g = self.tasks.get(name);
        let p = project.and_then(|p| p.tasks.get(name));
        if g.is_none() && p.is_none() {
            return None;
        }
        let pick = |f: fn(&TaskDef) -> Option<String>| p.and_then(f).or_else(|| g.and_then(f));
        Some((
            pick(|t| t.key.clone()).filter(|k| !k.is_empty()),
            pick(|t| t.label.clone()).unwrap_or_else(|| name.to_string()),
            p.and_then(|t| t.enabled)
                .or_else(|| g.and_then(|t| t.enabled))
                .unwrap_or(true),
        ))
    }

    /// Every task for the project, `Err` kept for the ones that do not resolve (menus grey
    /// them out with the reason).
    pub fn tasks_for(&self, project: Option<&Project>) -> Vec<(String, Result<ResolvedTask>)> {
        self.task_names(project)
            .into_iter()
            .map(|n| {
                let r = self.task(&n, project);
                (n, r)
            })
            .collect()
    }

    // ───────── templates ─────────

    /// The user's `init.lua` template: only what differs from the shipped defaults.
    pub fn example(projects: &[Project]) -> String {
        let mut s = String::from(
            r#"-- ~/.config/fleet/init.lua
-- Lua 5.4, sandboxed (string/table/math, os.getenv, require, read). This file is deep-merged
-- OVER fleet's shipped defaults: `fleet config --default` prints them, `fleet config
-- --effective` prints the result. Say only what differs.
local home = os.getenv("HOME")

return {
  -- 1. Your agents: a name of your choice -> which harness (opencode | claude | codex) and,
  --    optionally, a model in that harness's own naming. Keep only the ones you have installed.
  agents = {
    opencode = { harness = "opencode" },                      -- e.g. model = "anthropic/claude-opus-5-5"
    -- claude = { harness = "claude" },                       -- e.g. model = "opus"
    -- codex  = { harness = "codex" },                        -- e.g. model = "gpt-5.4"
  },

  -- 2. Which agent new tasks use (one of the names above) and the mode they start in.
  defaults = { agent = "opencode", mode = "plan" },          -- plan | build | yolo

  -- tasks: presets for starting an agent (start, review, explain ship by default).
  -- Keep them in tasks/mod.lua next to their prompt files: tasks = require("tasks"),
  -- tasks = {
  --   start  = { skills = { home .. "/.config/opencode/skills/implement-change/SKILL.md" } },
  --   review = { skills = { home .. "/.config/opencode/skills/review-team-mr/SKILL.md" } },
  -- },

  -- fleet:projects (fleet project add appends here)
  projects = {
"#,
        );
        for p in projects {
            s.push_str(&project_block(p));
        }
        s.push_str(
            r#"  },
  -- fleet:projects:end
  -- A project may override any section: defaults = {...}, agents = {...}, tasks = {...}, keys = {...}

  -- keys = { ["tab.reviews"] = "5" },   -- `fleet keys` lists every action
}
"#,
        );
        s
    }
}

/// The shipped defaults alone, for callers that run before any user file exists.
impl Default for Config {
    fn default() -> Self {
        Self::shipped().expect("shipped config is valid")
    }
}

/// Render one project as a Lua table literal (used by `--init` and `project add`).
pub fn project_block(p: &Project) -> String {
    let home = home().to_string_lossy().into_owned();
    let path = |x: &Path| {
        let s = x.to_string_lossy();
        match s.strip_prefix(&home) {
            Some(rest) => format!("home .. {:?}", rest),
            None => format!("{s:?}"),
        }
    };
    let mut s = format!(
        "    {{\n      name = {:?},\n      repo = {},\n",
        p.name,
        path(&p.repo)
    );
    if let Some(w) = &p.worktrees {
        s.push_str(&format!("      worktrees = {},\n", path(w)));
    }
    if let Some(r) = &p.reviews {
        s.push_str(&format!("      reviews = {r:?},\n"));
    }
    s.push_str(&format!("      base = {:?},\n    }},\n", p.base));
    s
}

/// `git@gitlab.com:g/p.git` / `https://github.com/o/r` -> `gitlab:g/p` / `github:o/r`.
pub fn review_source_from_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");
    let (host, path) = if let Some(rest) = url.strip_prefix("git@") {
        rest.split_once(':')?
    } else {
        let no_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
        no_scheme.split_once('/')?
    };
    let kind = match host {
        "gitlab.com" => "gitlab",
        "github.com" => "github",
        _ => return None,
    };
    Some(format!("{kind}:{}", path.trim_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(extra: &str) -> Project {
        Project {
            name: "svc".into(),
            repo: "/r".into(),
            worktrees: None,
            reviews: None,
            base: "main".into(),
            defaults: DefaultsPatch::default(),
            agents: Default::default(),
            tasks: Default::default(),
            keys: Default::default(),
        }
        .with(extra)
    }
    impl Project {
        fn with(mut self, lua_fields: &str) -> Self {
            if lua_fields.is_empty() {
                return self;
            }
            let c = Config::from_lua(
                &format!(
                    r#"return {{ projects = {{ {{ name = "svc", repo = "/r", {lua_fields} }} }} }}"#
                ),
                "t",
            )
            .unwrap();
            let p = c.projects.into_iter().next().unwrap();
            self.defaults = p.defaults;
            self.agents = p.agents;
            self.tasks = p.tasks;
            self.keys = p.keys;
            self
        }
    }

    /// The first thing a new user pastes (README › Getting started).
    const FIRST_CONFIG: &str = r#"return {
        agents = { opencode = { harness = "opencode" } },
        defaults = { agent = "opencode" },
    }"#;

    #[test]
    fn no_user_file_boots_but_warns_about_the_missing_agent() {
        let c = Config::shipped().unwrap();
        assert_eq!(c.defaults.agent, "");
        assert_eq!(c.defaults.mode, "plan");
        assert!(c.defaults.system.contains("{slug}"));
        assert!(c.enabled_agents().is_empty());
        assert!(
            c.warnings.iter().any(|w| w.contains("no agent")),
            "{:?}",
            c.warnings
        );
        let e = c.task("start", None).unwrap_err().to_string();
        assert!(
            e.contains("no agent configured") && e.contains("harness"),
            "{e}"
        );
        assert_eq!(c.keys["leader"], "Space");
    }

    #[test]
    fn first_config_works() {
        let c = Config::from_lua(FIRST_CONFIG, "init.lua").unwrap();
        assert!(
            c.warnings.iter().all(|w| !w.contains("no agent")),
            "{:?}",
            c.warnings
        );
        let t = c.task("review", None).unwrap();
        assert_eq!(t.on, On::Review);
        assert_eq!(t.key.as_deref(), Some("r"));
        assert_eq!(t.agent.name, "opencode");
        assert!(t.instructions.unwrap().contains("root cause"));
        assert_eq!(c.task("explain", None).unwrap().mode, "plan");
    }

    #[test]
    fn user_file_merges_over_shipped() {
        let src = r#"
            local home = os.getenv("HOME")
            return {
              defaults = { agent = "opencode", mode = "build" },
              agents = { opencode = { model = "anthropic/claude-opus-5-5" },
                         fast = { harness = "opencode", model = "anthropic/claude-sonnet-4-5" } },
              tasks = { start = { skills = { "/tmp/does-not-exist/SKILL.md" } },
                        bugfix = { key = "b", mode = "build", prompt = "Fix it." } },
              projects = {
                { name = "svc", repo = home .. "/x", reviews = "gitlab:a/b" },
                { name = "cli", repo = "~/y", worktrees = "~/wt", base = "develop" },
              },
              keys = { ["agent.start"] = "C-n" },
            }"#;
        let c = Config::from_lua(src, "test.lua").unwrap();
        assert_eq!(c.defaults.mode, "build");
        assert_eq!(c.defaults.refresh_secs, 2); // shipped
        assert_eq!(c.projects[0].repo, home().join("x"));
        assert_eq!(c.projects[1].worktrees, Some(home().join("wt")));
        assert_eq!(c.projects[1].base, "develop");
        assert_eq!(c.keys["agent.start"], "C-n");
        assert_eq!(c.keys["quit"], "q"); // shipped key kept
        let oc = c.agent("opencode", None).unwrap();
        assert_eq!(oc.harness, "opencode");
        assert_eq!(oc.model.as_deref(), Some("anthropic/claude-opus-5-5"));
        assert_eq!(c.agent("fast", None).unwrap().harness, "opencode");
        let start = c.task("start", None).unwrap();
        assert_eq!(start.key.as_deref(), Some("s")); // shipped
        assert_eq!(
            start.skills,
            vec![PathBuf::from("/tmp/does-not-exist/SKILL.md")]
        );
        assert_eq!(start.mode, "build"); // defaults.mode
        let bug = c.task("bugfix", None).unwrap();
        assert_eq!(bug.on, On::Project);
        assert_eq!(bug.prompt.as_deref(), Some("Fix it."));
        assert_eq!(bug.agent.name, "opencode");
        // the missing skill file is a warning, not an error
        assert!(
            c.warnings.iter().any(|w| w.contains("does-not-exist")),
            "{:?}",
            c.warnings
        );
    }

    #[test]
    fn project_overrides_every_section() {
        let c = Config::from_lua(
            r#"return { defaults = { agent = "opencode" }, agents = { opencode = {}, claude = {} },
               projects = { { name = "svc", repo = "/r",
                 defaults = { agent = "claude", mode = "build", system = "Project rules." },
                 agents = { claude = { model = "opus" } },
                 tasks = { review = { skills = { "/s/SKILL.md" } }, release = { key = "R" } },
                 keys = { ["tab.reviews"] = "5" } } } }"#,
            "t",
        )
        .unwrap();
        let p = c.project("svc").unwrap();
        let d = c.defaults_for(Some(p));
        assert_eq!(d.agent, "claude");
        assert_eq!(d.system, "Project rules.");
        assert_eq!(c.defaults_for(None).agent, "opencode");
        assert_eq!(
            c.agent("claude", Some(p)).unwrap().model.as_deref(),
            Some("opus")
        );
        assert_eq!(c.agent("claude", None).unwrap().model, None);
        let r = c.task("review", Some(p)).unwrap();
        assert_eq!(r.agent.name, "claude");
        assert_eq!(r.mode, "plan"); // the shipped review task pins plan
        assert_eq!(r.skills, vec![PathBuf::from("/s/SKILL.md")]);
        assert_eq!(r.system, "Project rules.");
        let rel = c.task("release", Some(p)).unwrap();
        assert_eq!(rel.key.as_deref(), Some("R"));
        assert!(c.task("release", None).is_err());
        assert_eq!(c.keys_for(Some(p))["tab.reviews"], "5");
        assert_eq!(c.keys_for(None)["tab.reviews"], "4");
    }

    #[test]
    fn agent_system_and_skills_accumulate() {
        let c = Config::from_lua(
            r#"return { defaults = { agent = "opencode" }, agents = { opencode = { skills = { "/a/SKILL.md" }, system = "Prefer small commits." } },
                        tasks = { start = { skills = { "/a/SKILL.md", "/b/SKILL.md" } } } }"#,
            "t",
        )
        .unwrap();
        let t = c.task("start", None).unwrap();
        assert_eq!(
            t.skills,
            vec![PathBuf::from("/a/SKILL.md"), PathBuf::from("/b/SKILL.md")]
        );
        assert!(t.system.ends_with("\nPrefer small commits."));
    }

    #[test]
    fn fatal_problems() {
        let err = |src: &str| Config::from_lua(src, "t").unwrap_err().to_string();
        let base = r#"agents = { opencode = { harness = "opencode" } }, defaults = { agent = "opencode" },"#;
        assert!(
            err(r#"return { agents = { opencode = {} }, defaults = { agent = "nope" } }"#)
                .contains("defaults.agent")
        );
        assert!(err(r#"return { agents = { x = { harness = "aider" } } }"#).contains("harness"));
        assert!(err(r#"return { agents = { x = { enabled = true } } }"#).contains("harness"));
        assert!(err(&format!(
            "return {{ {base} tasks = {{ t = {{ on = \"branch\" }} }} }}"
        ))
        .contains("on ="));
        assert!(err(&format!(
            "return {{ {base} tasks = {{ t = {{ agent = \"ghost\" }} }} }}"
        ))
        .contains("ghost"));
        assert!(err(&format!(
            "return {{ {base} tasks = {{ t = {{ key = \"s\" }} }} }}"
        ))
        .contains("key"));
        assert!(err(
            r#"return { projects = { { name = "a", repo = "/x" }, { name = "a", repo = "/y" } } }"#
        )
        .contains("twice"));
        assert!(err("return {").contains("expected"));
        assert!(err("x = 1").contains("return"));
    }

    #[test]
    fn warnings_not_errors() {
        let c = Config::from_lua(
            r#"return { defualts = { mode = "build" }, defaults = { agent = "codex" }, agents = { codex = { sandbox = "workspace-write" } },
                        tasks = { start = { skilz = {} } } }"#,
            "t",
        )
        .unwrap();
        let w = c.warnings.join("\n");
        assert!(w.contains("defualts"), "{w}");
        assert!(w.contains("defaults"), "{w}"); // suggestion
        assert!(w.contains("sandbox"), "{w}");
        assert!(w.contains("skilz"), "{w}");
    }

    #[test]
    fn legacy_shapes_still_parse() {
        let c = Config::from_lua(
            r#"return { defaults = { agent = "opencode", nvim_pane = true }, agent_args = { "--x" },
                        agents = { opencode = { enabled = true, args = {} } } }"#,
            "t",
        )
        .unwrap();
        assert_eq!(c.defaults.left_pane.as_deref(), Some("nvim"));
        assert_eq!(c.agent("opencode", None).unwrap().args, vec!["--x"]);
    }

    #[test]
    fn example_roundtrips() {
        let p = vec![proj("")];
        let src = Config::example(&p);
        let c = Config::from_lua(&src, "ex").unwrap();
        assert_eq!(c.projects.len(), 1);
        assert_eq!(c.projects[0].name, "svc");
        let a = c.agent("opencode", None).unwrap();
        assert_eq!(a.harness, "opencode");
        assert_eq!(c.defaults.agent, "opencode");
        assert!(
            c.warnings.iter().all(|w| !w.contains("no agent")),
            "{:?}",
            c.warnings
        );
    }

    #[test]
    fn remotes() {
        assert_eq!(
            review_source_from_remote("git@gitlab.com:monid-ai/monid-services.git").unwrap(),
            "gitlab:monid-ai/monid-services"
        );
        assert_eq!(
            review_source_from_remote("https://github.com/o/r").unwrap(),
            "github:o/r"
        );
        assert_eq!(review_source_from_remote("https://bitbucket.org/o/r"), None);
    }
}
