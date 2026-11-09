//! `fleet agent start`: worktree + branch + tmux + agent.

use super::{free_port, Progress};
use crate::agents::{LaunchSpec, Mode};
use crate::config::{Config, Project};
use crate::registry::{Registry, Task};
use crate::slug;
use crate::snapshot::Engine;
use crate::tmux;
use crate::worktrees::git;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct StartOpts {
    pub name: Option<String>,
    pub prompt: String,
    pub repo: String,
    /// Task preset (`tasks.<name>`); `None` = `start`.
    pub task: Option<String>,
    /// Agent name; `None` = the task's / defaults.agent.
    pub agent: Option<String>,
    /// `plan` | `build` | `yolo` | a harness mode; `None` = the task's / defaults.mode.
    pub mode: Option<String>,
    /// Model in the harness's naming; `None` = the task's / agent's, else the CLI's default.
    pub model: Option<String>,
    /// `Some(list)` replaces the task's skills (`Some(vec![])` = none).
    pub skills: Option<Vec<PathBuf>>,
    pub base: Option<String>,
    /// Override `defaults.left_pane` for this start: `Some(None)` = bare shell.
    pub left_pane: Option<Option<String>>,
    pub attach: bool,
}

#[derive(Debug, Clone)]
pub struct Started {
    pub agent: String,
    pub slug: String,
    pub branch: String,
    pub path: PathBuf,
    pub tmux_session: String,
    pub port: u16,
}

pub fn worktree_path(_cfg: &Config, repo: &Project, slug: &str) -> PathBuf {
    repo.worktrees_dir().join(slug)
}

pub async fn start(engine: &Engine, opts: StartOpts, log: &Progress) -> Result<Started> {
    let cfg = &engine.config;
    let repo = cfg
        .project(&opts.repo)
        .with_context(|| format!("repo `{}` is not in config", opts.repo))?;
    let prompt = opts.prompt.trim().to_string();
    if prompt.is_empty() {
        bail!("empty prompt");
    }
    let slug = match opts.name {
        Some(n) => {
            if !slug::is_valid(&n) {
                bail!("`{n}` is not a valid name (lowercase, digits, -, _, .)");
            }
            n
        }
        None => slug::temp(),
    };
    let branch = slug::branch_for(&slug);
    let base = opts.base.clone().unwrap_or_else(|| repo.base.clone());
    let path = worktree_path(cfg, repo, &slug);
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    if tmux::has_session(&slug).await {
        bail!("tmux session `{slug}` already exists");
    }
    let project = Some(repo);
    let task_name = opts.task.clone().unwrap_or_else(|| "start".into());
    let mut task = cfg.task(&task_name, project)?;
    if task.on != crate::config::On::Project {
        bail!("task `{task_name}` runs on a review; use `fleet task {task_name} <review-id>`");
    }
    if let Some(a) = &opts.agent {
        task.agent = cfg.agent(a, project).with_context(|| {
            format!(
                "agent `{a}` is not defined (agents: {})",
                cfg.agent_names(project).join(", ")
            )
        })?;
        if task.model.is_none() {
            task.model = task.agent.model.clone();
        }
    }
    if !task.agent.enabled {
        bail!("agent `{}` is disabled in config", task.agent.name);
    }
    if let Some(m) = &opts.mode {
        task.mode = m.clone();
    }
    if let Some(m) = &opts.model {
        task.model = Some(m.clone());
    }
    if let Some(sk) = &opts.skills {
        task.skills = sk.clone();
    }
    let agent_name = task.agent.name.clone();
    let adapter = engine
        .adapter_for_agent(&task.agent)
        .with_context(|| format!("unknown harness `{}`", task.agent.harness))?;
    let mode_s = task.mode.clone();
    let mode = Mode::parse(&mode_s).context("empty mode")?;
    let model = task.model.clone();
    // Check mode and model against what the harness offers in this repo. If discovery itself
    // fails (CLI slow or missing), go ahead: the CLI reports a bad name itself.
    match adapter.choices(&repo.repo).await {
        Ok(c) => c.check(Some(mode.as_str()), model.as_deref())?,
        Err(e) => log(&format!(
            "could not list {agent_name} modes/models ({e:#}); not checking"
        )),
    }
    // Skills: link into the harness's skill dir when needed; collect the names.
    let mut skill_names = vec![];
    for sk in &task.skills {
        match adapter.prepare_skill(sk) {
            Ok(n) => skill_names.push(n),
            Err(e) => log(&format!("skill {}: {e:#}; not loaded", sk.display())),
        }
    }

    log(&format!("fetch origin {base}"));
    git::fetch(&repo.repo, "origin", &base).await?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    log(&format!(
        "worktree add -b {branch} {} origin/{base}",
        path.display()
    ));
    git::worktree_add_branch(&repo.repo, &path, &branch, &format!("origin/{base}")).await?;

    let port = free_port()?;
    let facts = super::prompt::Facts {
        slug: slug.clone(),
        repo: repo.name.clone(),
        branch: branch.clone(),
        base: base.clone(),
        agent: agent_name.clone(),
        temp_slug: slug::is_temp(&slug),
        ..Default::default()
    };
    let system = super::prompt::system(&task, &facts, &skill_names);
    let reg = Registry::open()?;
    reg.insert(&Task {
        slug: slug.clone(),
        kind: "start".into(),
        repo: repo.name.clone(),
        branch: Some(branch.clone()),
        path: path.clone(),
        old_paths: vec![],
        tmux_session: slug.clone(),
        port: Some(port),
        agent: agent_name.clone(),
        mode: Some(mode_s.clone()),
        task: Some(task_name.clone()),
        review_source: None,
        review_id: None,
        head_sha: None,
        first_prompt: Some(super::prompt::combined(&system, &prompt)),
        created_at: Utc::now(),
        closed_at: None,
    })?;

    let cmd = adapter.launch(&LaunchSpec {
        cwd: path.clone(),
        mode,
        model,
        system,
        prompt,
        port,
    });

    log(&format!("tmux new-session -s {slug}"));
    tmux::new_session(&slug, &path).await?;
    // Always two panes: a shell on the left (pane 0), the agent on the right (pane 1).
    tmux::split_window(&format!("{slug}:1"), &path, None).await?;
    let left = opts
        .left_pane
        .clone()
        .unwrap_or_else(|| cfg.defaults.left_pane.clone());
    if let Some(c) = left.filter(|c| !c.is_empty()) {
        tmux::send_keys(&format!("{slug}:1.0"), &c).await.ok();
    }
    let agent_target = format!("{slug}:1.1");
    tmux::send_keys(&agent_target, &cmd).await?;
    tmux::select_pane(&agent_target).await.ok();

    if opts.attach {
        tmux::attach(&slug).await?;
    }
    Ok(Started {
        agent: agent_name,
        tmux_session: slug.clone(),
        slug,
        branch,
        path,
        port,
    })
}
