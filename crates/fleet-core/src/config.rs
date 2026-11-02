//! `~/.config/fleet/config.toml`

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub worktrees_dir: PathBuf,
    pub review_ttl_secs: u64,
    pub refresh_secs: u64,
    pub default_mode: String,
    pub nvim_pane: bool,
    pub notify: bool,
    /// Extra command-line arguments for the agent (e.g. `["--model", "anthropic/claude-opus-5"]`).
    pub agent_args: Vec<String>,
    pub repos: Vec<RepoConfig>,
    pub agents: AgentsConfig,
    /// action -> key chord, e.g. `"agent.start" = "a s"`.
    pub keys: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    pub path: PathBuf,
    /// `gitlab:group/project`. Optional: a repo with no review source is still tracked.
    #[serde(default)]
    pub review_source: Option<String>,
    /// Default base branch for new work. Defaults to `main`.
    #[serde(default = "default_base")]
    pub base: String,
}

fn default_base() -> String {
    "main".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    pub adapters: Vec<String>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            adapters: vec!["opencode".into()],
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            worktrees_dir: data_dir().join("worktrees"),
            review_ttl_secs: 60,
            refresh_secs: 2,
            default_mode: "plan".into(),
            nvim_pane: true,
            notify: true,
            agent_args: vec![],
            repos: vec![],
            agents: AgentsConfig::default(),
            keys: BTreeMap::new(),
        }
    }
}

/// XDG-style on every platform (macOS included): `$XDG_CONFIG_HOME/fleet` or `~/.config/fleet`.
/// This matches opencode, plannotator and tmux on this machine; `~/Library/Application Support`
/// is deliberately not used.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("fleet")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// `$XDG_DATA_HOME/fleet` or `~/.local/share/fleet`.
pub fn data_dir() -> PathBuf {
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

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let mut cfg: Config = match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        cfg.worktrees_dir = expand(&cfg.worktrees_dir);
        for r in &mut cfg.repos {
            r.path = expand(&r.path);
        }
        Ok(cfg)
    }

    pub fn repo_by_name(&self, name: &str) -> Option<&RepoConfig> {
        self.repos.iter().find(|r| r.name == name)
    }

    /// The configured repo that contains `path` (by canonical prefix), if any.
    pub fn repo_for_path(&self, path: &Path) -> Option<&RepoConfig> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.repos.iter().find(|r| {
            let rc = r.path.canonicalize().unwrap_or_else(|_| r.path.clone());
            canon.starts_with(&rc)
        })
    }

    pub fn example() -> String {
        r#"# fleet configuration
worktrees_dir = "~/.local/share/fleet/worktrees"
review_ttl_secs = 60
refresh_secs = 2
default_mode = "plan"      # opencode agent to start with: plan | build
nvim_pane = true           # open an nvim pane next to the agent
notify = true              # macOS banner when an agent starts waiting for you
agent_args = []            # extra args for `opencode`, e.g. ["--model", "anthropic/claude-opus-5"]

[[repos]]
name = "monid-services"
path = "~/Projects/monid/monid-services"
review_source = "gitlab:monid-ai/monid-services"
base = "main"

[agents]
adapters = ["opencode"]

[keys]
# action = "key" or "prefix key"; see `fleet keys` for every action
# "agent.start" = "a s"
"#
        .into()
    }
}
