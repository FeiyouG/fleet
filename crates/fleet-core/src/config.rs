//! `~/.config/fleet/init.lua` — a Lua file that *returns* the config table (nvim convention).
//!
//! Lua 5.4 is embedded (mlua, vendored) with a restricted standard library: string, table,
//! math, plus `os.getenv`. No `io`, no `os.execute`, no `require`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default = "Config::empty")]
pub struct Config {
    pub defaults: Defaults,
    /// (legacy) extra arguments for opencode; prefer `agents.opencode.args`.
    #[serde(deserialize_with = "lua_string_list")]
    pub agent_args: Vec<String>,
    pub projects: Vec<Project>,
    pub agents: AgentsConfig,
    /// action -> key chord, e.g. `["agent.start"] = "a s"`.
    pub keys: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// Adapter preselected by `fleet agent start` when `--agent` is omitted. REQUIRED in the
    /// config file (no hidden fallback): `validate` rejects an empty value.
    pub agent: String,
    pub mode: String,
    /// A task's tmux window is split: a plain shell on the LEFT, the agent on the RIGHT.
    /// `left_pane` is a command typed into that shell at start (e.g. `"nvim"`); unset or empty
    /// = leave it as a bare prompt. Replaces the old `nvim_pane = true|false` (still accepted:
    /// `true` means `"nvim"`).
    #[serde(default, deserialize_with = "left_pane_compat", alias = "nvim_pane")]
    pub left_pane: Option<String>,
    pub notify: bool,
    pub refresh_secs: u64,
    pub review_ttl_secs: u64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            agent: String::new(),
            mode: "plan".into(),
            left_pane: None,
            notify: true,
            refresh_secs: 2,
            review_ttl_secs: 60,
        }
    }
}

/// One repo you work in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// The main clone.
    pub repo: PathBuf,
    /// Where `fleet agent start` creates worktrees for this project. Defaults to
    /// `~/.local/share/fleet/worktrees/<name>`. Discovery lists every worktree git knows
    /// for the repo, wherever it lives; this only decides where new ones go.
    #[serde(default)]
    pub worktrees: Option<PathBuf>,
    /// `gitlab:group/project` or `github:owner/repo`. Optional: a project without one is
    /// still tracked, it just has no Reviews.
    #[serde(default)]
    pub reviews: Option<String>,
    /// Base branch for new work. Defaults to `main`.
    #[serde(default = "default_base")]
    pub base: String,
}

fn default_base() -> String {
    "main".into()
}

impl Project {
    pub fn worktrees_dir(&self) -> PathBuf {
        self.worktrees
            .clone()
            .unwrap_or_else(|| data_dir().join("worktrees").join(&self.name))
    }
}

/// One coding-agent adapter's settings. Independent of projects: any enabled agent can run
/// in any project.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub enabled: bool,
    /// Executable name or path. Defaults to the adapter name.
    pub bin: Option<String>,
    /// Extra arguments appended to every launch. For the model use `model`.
    #[serde(deserialize_with = "lua_string_list")]
    pub args: Vec<String>,
    /// Model for new agents, in this CLI's own naming (opencode `provider/model`, claude
    /// `opus`, codex `gpt-5.4`). `None` = the CLI's own default.
    pub model: Option<String>,
    /// Codex: `--sandbox` value for build mode (`workspace-write` by default).
    pub sandbox: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: None,
            args: vec![],
            model: None,
            sandbox: None,
        }
    }
}

/// `agents = { opencode = {...}, claude = {...} }`. The v0.2 shape
/// `agents = { adapters = {"opencode"} }` still parses.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AgentsConfig(pub BTreeMap<String, AgentConfig>);

impl AgentsConfig {
    pub fn enabled(&self) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(_, c)| c.enabled)
            .map(|(n, _)| n.as_str())
            .collect()
    }
    pub fn get(&self, name: &str) -> AgentConfig {
        self.0.get(name).cloned().unwrap_or_default()
    }
    pub fn builtin() -> Self {
        Self(
            ["opencode", "claude", "codex"]
                .iter()
                .map(|n| (n.to_string(), AgentConfig::default()))
                .collect(),
        )
    }
}

impl<'de> Deserialize<'de> for AgentsConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Legacy { adapters: Vec<String> },
            Map(BTreeMap<String, AgentConfig>),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Legacy { adapters } => AgentsConfig(
                adapters
                    .into_iter()
                    .map(|n| (n, AgentConfig::default()))
                    .collect(),
            ),
            Raw::Map(m) => AgentsConfig(m),
        })
    }
}

impl Config {
    /// What a file that omits a key gets: nothing. `validate` then insists on `agents` and
    /// `defaults.agent`, so the file is the whole truth.
    fn empty() -> Self {
        Self {
            defaults: Defaults::default(),
            agent_args: vec![],
            projects: vec![],
            agents: AgentsConfig::default(),
            keys: BTreeMap::new(),
        }
    }
}

/// The config used when NO file exists yet (so `fleet setup` / `fleet project add` can run):
/// every builtin agent enabled, opencode preselected. A file that exists must spell both out.
impl Default for Config {
    fn default() -> Self {
        Self {
            defaults: Defaults {
                agent: "opencode".into(),
                ..Defaults::default()
            },
            agent_args: vec![],
            projects: vec![],
            agents: AgentsConfig::builtin(),
            keys: BTreeMap::new(),
        }
    }
}

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

// ───────────────────────── paths ─────────────────────────

/// XDG-style on every platform (macOS included): `$XDG_CONFIG_HOME/fleet` or `~/.config/fleet`.
/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn xdg_config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("fleet")
}

pub fn config_path() -> PathBuf {
    config_dir().join("init.lua")
}

/// `$XDG_DATA_HOME/fleet` or `~/.local/share/fleet`.
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
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut cfg = Self::from_lua(&src, &path.to_string_lossy())?;
        cfg.normalise();
        Ok(cfg)
    }

    /// Run the Lua source in a sandbox and convert the returned table.
    pub fn from_lua(src: &str, chunk_name: &str) -> Result<Self> {
        use mlua::{Lua, LuaSerdeExt, StdLib, Value};
        let run = || -> mlua::Result<Option<Config>> {
            let lua = Lua::new_with(
                StdLib::STRING | StdLib::TABLE | StdLib::MATH,
                mlua::LuaOptions::default(),
            )?;
            // A tiny `os` with only getenv.
            let os = lua.create_table()?;
            os.set(
                "getenv",
                lua.create_function(|_, k: String| Ok(std::env::var(k).ok()))?,
            )?;
            lua.globals().set("os", os)?;
            let value: Value = lua.load(src).set_name(chunk_name).eval()?;
            if matches!(value, Value::Nil) {
                return Ok(None);
            }
            Ok(Some(lua.from_value(value)?))
        };
        let cfg = match run() {
            Ok(Some(cfg)) => cfg,
            Ok(None) => bail!("{chunk_name}: the file must `return {{ ... }}` a config table"),
            Err(e) => bail!("{chunk_name}: {}", lua_error(e)),
        };
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("{chunk_name}: {e}"))?;
        Ok(cfg)
    }

    /// Everything about agents is explicit in the file: which adapters exist (and whether they
    /// are enabled) and which one `fleet agent start` preselects. Nothing is inherited silently,
    /// so a config written for one machine reads the same on another.
    pub fn validate(&self) -> Result<()> {
        let known = crate::agents::KNOWN;
        if self.agents.0.is_empty() {
            bail!(
                "`agents` is not set. Declare every adapter explicitly, e.g.\n  agents = {{\n    \
                 opencode = {{ enabled = true }},\n    claude = {{ enabled = true }},\n    \
                 codex = {{ enabled = false }},\n  }}\n(known adapters: {})",
                known.join(", ")
            );
        }
        if let Some(bad) = self.agents.0.keys().find(|n| !known.contains(&n.as_str())) {
            bail!(
                "agents.{bad}: unknown adapter (known: {})",
                known.join(", ")
            );
        }
        let enabled = self.agents.enabled();
        if enabled.is_empty() {
            bail!("every adapter in `agents` is disabled; enable at least one");
        }
        if self.defaults.agent.is_empty() {
            bail!(
                "defaults.agent is not set. Add e.g. defaults = {{ agent = \"{}\" }} (enabled: {})",
                enabled[0],
                enabled.join(", ")
            );
        }
        if !enabled.contains(&self.defaults.agent.as_str()) {
            bail!(
                "defaults.agent = \"{}\" is not an enabled adapter (enabled: {})",
                self.defaults.agent,
                enabled.join(", ")
            );
        }
        // plan | build work for every adapter; other names (an opencode agent) are checked
        // against the adapter's own list when an agent starts.
        if crate::agents::Mode::parse(&self.defaults.mode).is_none() {
            bail!("defaults.mode is empty; use plan, build, or an opencode agent name");
        }
        Ok(())
    }

    fn normalise(&mut self) {
        for p in &mut self.projects {
            p.repo = expand(&p.repo);
            if let Some(w) = &p.worktrees {
                p.worktrees = Some(expand(w));
            }
        }
    }

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

    pub fn example(projects: &[Project]) -> String {
        let mut s = String::from(
            r#"-- ~/.config/fleet/init.lua
-- Lua 5.4, sandboxed: string/table/math + os.getenv. The file must return the config table.
local home = os.getenv("HOME")

return {
  defaults = {
    agent = "opencode",     -- adapter preselected by `fleet agent start`: opencode | claude | codex
    mode = "plan",          -- plan (read-only) | build, or an opencode agent (`fleet agent choices`)
    -- left_pane = "nvim", -- command for the LEFT (shell) pane of a new task; unset = bare shell
    notify = true,          -- macOS banner when an agent starts waiting for you
    refresh_secs = 2,
    review_ttl_secs = 60,
  },
  agent_args = {},          -- (legacy) extra args for opencode; prefer agents.opencode.args

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

  -- Agents: which coding-agent CLIs fleet discovers and can launch. Independent of projects:
  -- any enabled agent can run in any project. Both this table and defaults.agent are REQUIRED:
  -- fleet never assumes an adapter you did not write down.
  agents = {
    -- args are appended to every launch; model = the model for new agents in that CLI's naming
    -- (unset = the CLI's own default; `fleet agent choices --agent NAME` lists what it offers)
    opencode = { enabled = true, args = {} },          -- e.g. model = "anthropic/claude-opus-5-5"
    claude   = { enabled = true, args = {} },          -- e.g. model = "opus"
    codex    = { enabled = true, args = {}, sandbox = "workspace-write" },
  },

  keys = {
    -- action = "key" or "prefix key"; `fleet keys` lists every action
    -- ["agent.start"] = "C-n",
  },
}
"#,
        );
        s
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

fn lua_error(e: mlua::Error) -> String {
    match e {
        mlua::Error::SyntaxError { message, .. } => message,
        mlua::Error::RuntimeError(m) => m,
        other => other.to_string(),
    }
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

    #[test]
    fn loads_lua() {
        let src = r#"
            local home = os.getenv("HOME")
            return {
              defaults = { agent = "opencode", mode = "build" },
              agents = { opencode = { enabled = true } },
              projects = {
                { name = "svc", repo = home .. "/x", reviews = "gitlab:a/b" },
                { name = "cli", repo = "~/y", worktrees = "~/wt", base = "develop" },
              },
              keys = { ["agent.start"] = "C-n" },
            }"#;
        let mut c = Config::from_lua(src, "test.lua").unwrap();
        c.normalise();
        assert_eq!(c.defaults.mode, "build");
        assert_eq!(c.defaults.refresh_secs, 2);
        assert_eq!(c.projects.len(), 2);
        assert_eq!(c.projects[0].repo, home().join("x"));
        assert_eq!(c.projects[1].repo, home().join("y"));
        assert_eq!(c.projects[1].worktrees, Some(home().join("wt")));
        assert_eq!(c.projects[1].base, "develop");
        assert_eq!(
            c.projects[0].worktrees_dir(),
            data_dir().join("worktrees").join("svc")
        );
        assert_eq!(c.keys["agent.start"], "C-n");
    }

    #[test]
    fn left_pane_shapes() {
        let c = |extra: &str| {
            Config::from_lua(
                &format!(
                    r#"return {{ defaults = {{ agent = "opencode" {extra} }}, agents = {{ opencode = {{}} }} }}"#
                ),
                "t",
            )
            .unwrap()
            .defaults
            .left_pane
        };
        assert_eq!(c(""), None);
        assert_eq!(c(r#", left_pane = "nvim""#), Some("nvim".into()));
        assert_eq!(c(r#", left_pane = """#), None);
        assert_eq!(c(", nvim_pane = true"), Some("nvim".into()));
        assert_eq!(c(", nvim_pane = false"), None);
    }

    #[test]
    fn agents_table_both_shapes() {
        let c = Config::from_lua(
            r#"return { defaults = { agent = "opencode" }, agents = { adapters = { "opencode" } } }"#,
            "t",
        )
        .unwrap();
        assert_eq!(c.agents.enabled(), vec!["opencode"]);
        let c = Config::from_lua(r#"return { defaults = { agent = "claude", mode = "reviewer" }, agents = { claude = { args = { "--verbose" }, model = "opus" }, codex = { enabled = false } } }"#, "t").unwrap();
        assert_eq!(c.agents.enabled(), vec!["claude"]);
        assert_eq!(c.agents.get("claude").args, vec!["--verbose"]);
        assert_eq!(c.agents.get("claude").model.as_deref(), Some("opus"));
        assert_eq!(c.agents.get("opencode").model, None);
        assert_eq!(c.defaults.mode, "reviewer");
        // No file at all: builtin set, opencode preselected (bootstrap only).
        assert_eq!(Config::default().agents.enabled().len(), 3);
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn agents_must_be_explicit() {
        let err = |src: &str| Config::from_lua(src, "t").unwrap_err().to_string();
        assert!(err("return {}").contains("`agents` is not set"));
        assert!(
            err(r#"return { agents = { opencode = {} } }"#).contains("defaults.agent is not set")
        );
        assert!(
            err(r#"return { defaults = { agent = "codex" }, agents = { opencode = {} } }"#)
                .contains("not an enabled adapter")
        );
        assert!(err(r#"return { defaults = { agent = "opencode" }, agents = { opencode = {}, aider = {} } }"#)
            .contains("unknown adapter"));
        assert!(err(r#"return { defaults = { agent = "opencode" }, agents = { opencode = { enabled = false } } }"#)
            .contains("enable at least one"));
    }

    #[test]
    fn sandbox_and_errors() {
        assert!(Config::from_lua("return { projects = io.open('x') }", "t")
            .unwrap_err()
            .to_string()
            .contains("io"));
        let e = Config::from_lua("return { projects = 5 }", "t")
            .unwrap_err()
            .to_string();
        assert!(e.contains("expected table"), "{e}");
        assert!(Config::from_lua("return {", "t").is_err());
        assert!(Config::from_lua("x = 1", "t")
            .unwrap_err()
            .to_string()
            .contains("return"));
    }

    #[test]
    fn example_roundtrips() {
        let p = vec![Project {
            name: "svc".into(),
            repo: home().join("Projects/svc"),
            worktrees: None,
            reviews: Some("gitlab:a/b".into()),
            base: "main".into(),
        }];
        let src = Config::example(&p);
        let c = Config::from_lua(&src, "ex").unwrap();
        assert_eq!(c.projects[0].name, "svc");
        assert_eq!(c.projects[0].repo, home().join("Projects/svc"));
    }

    #[test]
    fn remotes() {
        assert_eq!(
            review_source_from_remote("git@gitlab.com:monid-ai/monid-services.git").unwrap(),
            "gitlab:monid-ai/monid-services"
        );
        assert_eq!(
            review_source_from_remote("git@github.com:monid-ai/cli.git").unwrap(),
            "github:monid-ai/cli"
        );
        assert_eq!(
            review_source_from_remote("https://github.com/o/r").unwrap(),
            "github:o/r"
        );
        assert_eq!(review_source_from_remote("https://bitbucket.org/o/r"), None);
    }
}
