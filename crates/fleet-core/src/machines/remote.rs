//! Running fleet's tmux and git commands on a machine, plus the one-time machine setup.
//!
//! These are the same commands the local helpers (`tmux.rs`, `worktrees/git.rs`) run, sent
//! through [`MachineAdapter::exec`]. tmux is required on every machine; `prepare` installs it
//! when missing and sets `base-index 1` (panes stay 0-based) so `<slug>:1.0` is the shell and
//! `<slug>:1.1` the agent, as fleet addresses them on the laptop.

use super::interfaces::{ExecOut, MachineAdapter, MachineRow, MachineSpec, MachineState, Size};
use crate::config::MachineConfig;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a profile keeps repos and worktrees (InstaBox HOME is `/data/home`).
pub const DEFAULT_WORKDIR: &str = "/data/home/src";

/// A machine plus the id of one machine of it: what commands run on.
pub struct On<'a> {
    pub adapter: &'a dyn MachineAdapter,
    pub id: String,
}

impl On<'_> {
    pub async fn exec(&self, argv: &[&str], cwd: Option<&Path>) -> Result<ExecOut> {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        self.adapter.exec(&self.id, &argv, cwd).await
    }

    /// Run and return stdout; a non-zero exit is an error naming the command.
    pub async fn run(&self, argv: &[&str], cwd: Option<&Path>) -> Result<String> {
        let out = self.exec(argv, cwd).await?;
        out.ok(&argv.join(" "))
    }

    pub async fn bash(&self, script: &str) -> Result<String> {
        self.run(&["bash", "-lc", script], None).await
    }

    // ── tmux (same arguments as `crate::tmux`) ──

    pub async fn has_session(&self, name: &str) -> bool {
        self.exec(&["tmux", "has-session", "-t", &format!("={name}")], None)
            .await
            .map(|o| o.success())
            .unwrap_or(false)
    }

    pub async fn new_session(&self, name: &str, cwd: &Path) -> Result<()> {
        let c = cwd.to_string_lossy();
        self.run(&["tmux", "new-session", "-d", "-s", name, "-c", &c], None)
            .await
            .map(|_| ())
    }

    pub async fn split_window(&self, target: &str, cwd: &Path) -> Result<()> {
        let c = cwd.to_string_lossy();
        self.run(
            &["tmux", "split-window", "-h", "-t", target, "-c", &c],
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn send_keys(&self, target: &str, text: &str) -> Result<()> {
        self.run(&["tmux", "send-keys", "-t", target, "-l", text], None)
            .await?;
        self.run(&["tmux", "send-keys", "-t", target, "Enter"], None)
            .await
            .map(|_| ())
    }

    pub async fn select_pane(&self, target: &str) -> Result<()> {
        self.run(&["tmux", "select-pane", "-t", target], None)
            .await
            .map(|_| ())
    }

    // ── git (same arguments as `crate::worktrees::git`) ──

    pub async fn fetch(&self, repo: &Path, remote: &str, refspec: &str) -> Result<()> {
        self.run(&["git", "fetch", "--quiet", remote, refspec], Some(repo))
            .await
            .map(|_| ())
    }

    pub async fn worktree_add_branch(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
        start: &str,
    ) -> Result<()> {
        let p = path.to_string_lossy();
        self.run(
            &["git", "worktree", "add", "-b", branch, &p, start],
            Some(repo),
        )
        .await
        .map(|_| ())
    }

    pub async fn worktree_add_detached(&self, repo: &Path, path: &Path, rev: &str) -> Result<()> {
        let p = path.to_string_lossy();
        self.run(&["git", "worktree", "add", "--detach", &p, rev], Some(repo))
            .await
            .map(|_| ())
    }
}

/// Paths of a project on a machine.
pub struct Layout {
    pub workdir: PathBuf,
    /// The main clone.
    pub repo: PathBuf,
}

impl Layout {
    pub fn new(cfg: &MachineConfig, project: &str) -> Self {
        let workdir = PathBuf::from(cfg.workdir.as_deref().unwrap_or(DEFAULT_WORKDIR));
        Self {
            repo: workdir.join(project),
            workdir,
        }
    }
    pub fn worktree(&self, slug: &str) -> PathBuf {
        self.workdir.join("wt").join(slug)
    }
}

/// `git@host:group/repo.git` / `ssh://git@host/group/repo` -> `https://host/group/repo.git`.
/// Machines reach forges over HTTPS with a proxied token, never with your SSH key.
pub fn https_url(origin: &str) -> String {
    let o = origin.trim();
    if o.starts_with("https://") {
        return o.to_string();
    }
    let rest = o
        .strip_prefix("ssh://")
        .map(|r| {
            r.split_once('@')
                .map(|(_, h)| h)
                .unwrap_or(r)
                .replacen('/', ":", 1)
        })
        .or_else(|| o.split_once('@').map(|(_, h)| h.to_string()))
        .unwrap_or_else(|| o.to_string());
    let (host, path) = rest.split_once(':').unwrap_or((&rest, ""));
    // `host:2222:path` from an ssh:// URL with a port: drop the port.
    let path = match path.split_once(':') {
        Some((port, p)) if port.chars().all(|c| c.is_ascii_digit()) => p,
        _ => path,
    };
    let path = path.trim_start_matches('/');
    let path = if path.ends_with(".git") {
        path.to_string()
    } else {
        format!("{path}.git")
    };
    format!("https://{host}/{path}")
}

/// One-time, idempotent machine setup for a project: tmux (window base-index 1), git
/// credentials that read the proxied tokens, and the main clone.
pub async fn prepare(on: &On<'_>, layout: &Layout, origin: &str) -> Result<()> {
    let url = https_url(origin);
    let q = |p: &Path| shell_words::quote(&p.to_string_lossy()).into_owned();
    let script = format!(
        r#"set -e
command -v tmux >/dev/null 2>&1 || {{ sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq tmux >/dev/null; }}
[ -f ~/.tmux.conf ] || printf "set -g base-index 1\n" > ~/.tmux.conf
if [ -n "$GITLAB_TOKEN" ]; then git config --global credential.https://gitlab.com.helper '!f() {{ echo username=oauth2; echo "password=$GITLAB_TOKEN"; }}; f'; fi
if [ -n "${{GITHUB_TOKEN:-$GH_TOKEN}}" ]; then git config --global credential.https://github.com.helper '!f() {{ echo username=x-access-token; echo "password=${{GITHUB_TOKEN:-$GH_TOKEN}}"; }}; f'; fi
mkdir -p {wt}
[ -d {repo}/.git ] || git clone --quiet {url} {repo}
"#,
        wt = q(&layout.workdir.join("wt")),
        repo = q(&layout.repo),
        url = shell_words::quote(&url),
    );
    on.bash(&script)
        .await
        .with_context(|| format!("preparing {} for {}", on.id, layout.repo.display()))?;
    Ok(())
}

/// What the in-machine fleet needs to know about itself (`~/.config/fleet/machine.json`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MachineFile {
    pub id: String,
    pub provider: String,
    pub api_url: Option<String>,
    pub api_key_env: String,
    pub workdir: String,
    pub publish_secs: u64,
}

impl MachineFile {
    pub fn path() -> PathBuf {
        crate::config::config_dir().join("machine.json")
    }
    pub fn load() -> Result<Self> {
        let p = Self::path();
        let s = std::fs::read_to_string(&p)
            .with_context(|| format!("{} (not a fleet machine?)", p.display()))?;
        Ok(serde_json::from_str(&s)?)
    }
}

/// Give the machine its own fleet config (the agent and project of this task, merged with
/// what earlier tasks added) and start the state publisher when fleet is installed there
/// and the machine holds a provider key. Returns whether the publisher runs.
pub async fn configure_fleet(
    on: &On<'_>,
    cfg: &MachineConfig,
    layout: &Layout,
    project: &str,
    agent: &crate::config::ResolvedAgent,
) -> Result<bool> {
    let key_env = cfg
        .api_key_env
        .clone()
        .unwrap_or_else(|| "INSTABOX_API_KEY".into());
    let mf = MachineFile {
        id: on.id.clone(),
        provider: cfg.provider.clone(),
        api_url: cfg.api_url.clone(),
        api_key_env: key_env.clone(),
        workdir: layout.workdir.to_string_lossy().into_owned(),
        publish_secs: cfg.publish_secs,
    };
    let lua_str = |s: &str| format!("{s:?}");
    let agent_line = format!(
        "[{}] = {{ harness = {}, bin = {} }},\n",
        lua_str(&agent.name),
        lua_str(&agent.harness),
        lua_str(&agent.bin)
    );
    let project_line = format!(
        "{{ name = {}, repo = {}, worktrees = {} }},\n",
        lua_str(project),
        lua_str(&layout.repo.to_string_lossy()),
        lua_str(&layout.workdir.join("wt").to_string_lossy())
    );
    let q = |s: &str| shell_words::quote(s).into_owned();
    let script = format!(
        r#"set -e
d="${{XDG_CONFIG_HOME:-$HOME/.config}}/fleet"
mkdir -p "$d/agents" "$d/projects"
printf %s {agent_line} > "$d/agents/{agent_file}.lua"
printf %s {project_line} > "$d/projects/{project_file}.lua"
printf %s {machine_json} > "$d/machine.json"
first=$(ls "$d/agents" | head -1 | sed 's/\.lua$//')
{{ echo "return {{"; echo "  agents = {{"; cat "$d"/agents/*.lua; echo "  }},";
   echo "  defaults = {{ agent = \"$first\" }},"; echo "  projects = {{"; cat "$d"/projects/*.lua; echo "  }},"; echo "}}"; }} > "$d/init.lua"
if command -v fleet >/dev/null 2>&1 && [ -n "${{{key_env}:-}}" ]; then
  pgrep -f 'fleet machine publish' >/dev/null 2>&1 || {{ mkdir -p ~/.local/share/fleet; nohup fleet machine publish >~/.local/share/fleet/publish.log 2>&1 </dev/null & }}
  echo publishing
fi
"#,
        agent_line = q(&agent_line),
        project_line = q(&project_line),
        machine_json = q(&serde_json::to_string(&mf)?),
        agent_file = if crate::slug::is_valid(&agent.name) {
            agent.name.as_str()
        } else {
            "agent"
        },
        project_file = if crate::slug::is_valid(project) {
            project
        } else {
            "project"
        },
    );
    let out = on.bash(&script).await?;
    Ok(out.contains("publishing"))
}

/// What `fleet machine doctor` checks: (tool, present).
pub async fn doctor(on: &On<'_>, harness: &str) -> Result<Vec<(String, bool)>> {
    let mut out = vec![];
    for tool in ["tmux", "git", harness] {
        let ok = on
            .exec(&["bash", "-lc", &format!("command -v {tool}")], None)
            .await?
            .success();
        out.push((tool.to_string(), ok));
    }
    Ok(out)
}

/// Run a terminal command from [`MachineAdapter::tty_command`] for the user: in a new
/// window of your tmux when fleet runs inside tmux, else in this terminal.
pub async fn open_tty(cmd: &[String], title: &str) -> Result<()> {
    if cmd.is_empty() {
        bail!("empty terminal command");
    }
    if crate::tmux::inside_tmux() {
        let st = tokio::process::Command::new("tmux")
            .args(["new-window", "-n", title, &shell_words::join(cmd)])
            .status()
            .await
            .context("tmux new-window")?;
        if !st.success() {
            bail!("tmux new-window failed");
        }
        return Ok(());
    }
    let st = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .with_context(|| format!("running {}", cmd[0]))?;
    if !st.success() {
        bail!("{} exited with {st}", cmd[0]);
    }
    Ok(())
}

/// A create request from a profile.
pub fn spec_for(cfg: &MachineConfig, name: &str) -> MachineSpec {
    MachineSpec {
        name: name.to_string(),
        profile: cfg.name.clone(),
        template: cfg.template.clone(),
        snapshot: cfg.snapshot.clone(),
        size: cfg.size.map(|s| Size {
            vcpus: s.vcpus,
            mem_mib: s.mem_mib,
        }),
        idle_after: cfg.idle_min.map(|m| Duration::from_secs(m as u64 * 60)),
        max_run_min: cfg.max_run_min,
        secrets: cfg.secrets.clone(),
        files: cfg.files.clone(),
        env_secrets: cfg
            .publish_key
            .iter()
            .map(|k| {
                (
                    cfg.api_key_env
                        .clone()
                        .unwrap_or_else(|| "INSTABOX_API_KEY".into()),
                    k.clone(),
                )
            })
            .collect(),
        env: cfg.env.clone(),
        metadata: Default::default(),
    }
}

/// The machine a new agent `slug` runs on (spec: machines, "Machine lifecycle per profile"):
/// a new `<profile>-<slug>` for `per_task`, else a live machine of the profile (woken when
/// paused) or a new `<profile>-<n>`.
pub async fn acquire(
    cfg: &MachineConfig,
    adapter: &dyn MachineAdapter,
    slug: &str,
    log: &crate::cmd::Progress,
) -> Result<MachineRow> {
    let existing = adapter.list().await?;
    if cfg.per_task {
        let name = machine_name(&cfg.name, slug);
        if let Some(m) = existing.iter().find(|m| m.name == name) {
            bail!(
                "machine {name} already exists ({}); remove it first: fleet machine rm {name}",
                m.state.label()
            );
        }
        log(&format!("create machine {name} ({})", cfg.provider));
        return adapter.create(&spec_for(cfg, &name)).await;
    }
    let live = existing.iter().find(|m| {
        m.profile() == Some(cfg.name.as_str())
            && !matches!(m.state, MachineState::Gone | MachineState::Error { .. })
    });
    if let Some(m) = live {
        if !m.state.is_running() {
            log(&format!("resume machine {}", m.name));
            adapter.resume(&m.id).await?;
        }
        return adapter.get(&m.id).await;
    }
    let name = (1..)
        .map(|n| machine_name(&cfg.name, &n.to_string()))
        .find(|n| !existing.iter().any(|m| &m.name == n))
        .expect("an unused name");
    log(&format!("create machine {name} ({})", cfg.provider));
    adapter.create(&spec_for(cfg, &name)).await
}

/// The machine name for a per-task machine: `<profile>-<slug>`, cut to InstaBox's 63 chars.
pub fn machine_name(profile: &str, slug: &str) -> String {
    let mut n = format!("{profile}-{slug}");
    n.truncate(63);
    n.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_to_https() {
        assert_eq!(
            https_url("git@gitlab.com:monid-ai/monid-services.git"),
            "https://gitlab.com/monid-ai/monid-services.git"
        );
        assert_eq!(
            https_url("ssh://git@gitlab.com/monid-ai/monid-services"),
            "https://gitlab.com/monid-ai/monid-services.git"
        );
        assert_eq!(
            https_url("ssh://git@gitlab.com:2222/g/r.git"),
            "https://gitlab.com/g/r.git"
        );
        assert_eq!(
            https_url("https://github.com/FeiyouG/fleet.git"),
            "https://github.com/FeiyouG/fleet.git"
        );
    }

    #[test]
    fn names_and_layout() {
        assert_eq!(machine_name("ibx", "fix-a"), "ibx-fix-a");
        assert!(machine_name("ibx", &"x".repeat(80)).len() <= 63);
        let cfg = crate::config::MachineConfig {
            name: "ibx".into(),
            provider: "instabox".into(),
            api_key_env: None,
            api_url: None,
            template: None,
            snapshot: None,
            size: None,
            per_task: true,
            idle_min: None,
            max_run_min: None,
            secrets: vec![],
            files: Default::default(),
            env: Default::default(),
            workdir: None,
            mosh: false,
            publish_key: None,
            publish_secs: 10,
            fleet_source: String::new(),
            fleet_ref: String::new(),
            setup: vec![],
        };
        let l = Layout::new(&cfg, "monid-services");
        assert_eq!(l.repo, PathBuf::from("/data/home/src/monid-services"));
        assert_eq!(
            l.worktree("fix-a"),
            PathBuf::from("/data/home/src/wt/fix-a")
        );
    }
}
