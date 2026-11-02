//! `fleet agent start`: worktree + branch + tmux + agent.

use super::{free_port, Progress};
use crate::agents::LaunchSpec;
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
    pub mode: Option<String>,
    /// `provider/model`; `None` = `defaults.model`, else the agent CLI's own default.
    pub model: Option<String>,
    pub base: Option<String>,
    pub nvim: Option<bool>,
    pub attach: bool,
}

#[derive(Debug, Clone)]
pub struct Started {
    pub slug: String,
    pub branch: String,
    pub path: PathBuf,
    pub tmux_session: String,
    pub port: u16,
}

pub fn prompt_header(slug: &str, repo: &str, branch: &str, base: &str) -> String {
    let mut h =
        format!("Task: {slug} · Repo: {repo} · Branch: {branch} · Base: {base} · Started by fleet");
    if slug::is_temp(slug) {
        h.push_str(&format!(
            "\nBefore any other step, pick a short kebab-case name for this task (e.g. fix-sfs-content-type; \
             a leading feat-/fix-/update-/refactor-/docs-/chore-/test- becomes the branch type) and run: \
             fleet agent rename {slug} <name>"
        ));
    }
    h
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
    let adapter = engine
        .agent_adapter("opencode")
        .context("opencode adapter not enabled")?;
    let mode = opts
        .mode
        .clone()
        .unwrap_or_else(|| cfg.defaults.mode.clone());
    let model = opts.model.clone().or_else(|| cfg.defaults.model.clone());
    // Check the agent and model against what opencode offers in this repo. If discovery
    // itself fails (opencode slow or missing), go ahead: opencode reports a bad name itself.
    match adapter.choices(&repo.repo).await {
        Ok(c) => {
            c.check_agent(&mode)?;
            if let Some(m) = &model {
                c.check_model(m)?;
            }
        }
        Err(e) => log(&format!(
            "could not list agents/models ({e:#}); not checking"
        )),
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
        mode: Some(mode.clone()),
        review_source: None,
        review_id: None,
        head_sha: None,
        first_prompt: Some(prompt.clone()),
        created_at: Utc::now(),
        closed_at: None,
    })?;

    let full_prompt = format!(
        "{}\n\n{prompt}",
        prompt_header(&slug, &repo.name, &branch, &base)
    );
    let cmd = adapter.launch(&LaunchSpec {
        cwd: path.clone(),
        mode,
        model,
        prompt: full_prompt,
        port,
        extra_args: cfg.agent_args.clone(),
    });

    log(&format!("tmux new-session -s {slug}"));
    tmux::new_session(&slug, &path).await?;
    let agent_target = if opts.nvim.unwrap_or(cfg.defaults.nvim_pane) {
        tmux::split_window(&format!("{slug}:1"), &path, None).await?;
        tmux::send_keys(&format!("{slug}:1.0"), "nvim").await.ok();
        format!("{slug}:1.1")
    } else {
        format!("{slug}:1.0")
    };
    tmux::send_keys(&agent_target, &cmd).await?;
    tmux::select_pane(&agent_target).await.ok();

    if opts.attach {
        tmux::attach(&slug).await?;
    }
    Ok(Started {
        tmux_session: slug.clone(),
        slug,
        branch,
        path,
        port,
    })
}
