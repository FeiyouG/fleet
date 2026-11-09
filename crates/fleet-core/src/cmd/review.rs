//! `fleet review start ID|URL`: detached worktree at the review head + tmux + review agent.
//! Re-review when the task already exists.

use super::{free_port, Progress};
use crate::agents::{LaunchSpec, Mode, SendMsg};
use crate::config::Project;
use crate::registry::{Registry, Task};
use crate::snapshot::Engine;
use crate::tmux;
use crate::worktrees::git;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ReviewOpts {
    pub id_or_url: String,
    /// Task preset (`tasks.<name>`, an `on = "review"` one); `None` = `review`.
    pub task: Option<String>,
    /// Agent name; `None` = the task's / defaults.agent.
    pub agent: Option<String>,
    /// `Some(list)` replaces the task's skills.
    pub skills: Option<Vec<std::path::PathBuf>>,
    /// Repo name; when `None`, the first repo whose source parses the id/url is used.
    pub repo: Option<String>,
    pub attach: bool,
}

#[derive(Debug, Clone)]
pub enum ReviewOutcome {
    Started {
        slug: String,
        path: PathBuf,
        port: u16,
    },
    ReReviewed {
        slug: String,
        old_sha: String,
        new_sha: String,
    },
}

fn resolve_repo<'a>(engine: &'a Engine, opts: &ReviewOpts) -> Result<(&'a Project, u64)> {
    let cfg = &engine.config;
    let candidates: Vec<&Project> = match &opts.repo {
        Some(n) => vec![cfg
            .project(n)
            .with_context(|| format!("repo `{n}` not in config"))?],
        None => cfg.projects.iter().collect(),
    };
    for repo in candidates {
        let Some(src) = &repo.reviews else {
            continue;
        };
        let Some(adapter) = engine.review_adapter(src) else {
            continue;
        };
        if let Some(id) = adapter.parse_id(&opts.id_or_url) {
            // A bare number matches every repo; prefer the cwd's repo when ambiguous.
            return Ok((repo, id));
        }
    }
    bail!(
        "could not resolve `{}` to a review of a configured repo",
        opts.id_or_url
    )
}

pub async fn start(engine: &Engine, opts: ReviewOpts, log: &Progress) -> Result<ReviewOutcome> {
    let cfg = &engine.config;
    let (repo, id) = resolve_repo(engine, &opts)?;
    let source = repo.reviews.clone().unwrap();
    let adapter = engine.review_adapter(&source).context("review adapter")?;
    let review = adapter.review(id).await?;
    let reg = Registry::open()?;
    let project = Some(repo);
    let task_name = opts.task.clone().unwrap_or_else(|| "review".into());
    let mut task = cfg.task(&task_name, project)?;
    if task.on != crate::config::On::Review {
        bail!("task `{task_name}` runs on a project, not a review; use `fleet task {task_name} --repo …`");
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
    if let Some(sk) = &opts.skills {
        task.skills = sk.clone();
    }
    let agent_name = task.agent.name.clone();
    let agent = engine
        .adapter_for_agent(&task.agent)
        .with_context(|| format!("unknown harness `{}`", task.agent.harness))?;

    log(&format!("fetch origin {}", adapter.fetch_refspec(id)));
    git::fetch(&repo.repo, "origin", &adapter.fetch_refspec(id)).await?;

    if let Some(task) = reg.find_review(&source, id)? {
        // Re-review: same worktree, same session, new prompt.
        let old_sha = task.head_sha.clone().unwrap_or_default();
        // FETCH_HEAD belongs to the main repo, not the worktree: resolve it to a sha first.
        let sha = git::git(&repo.repo, &["rev-parse", "FETCH_HEAD"])
            .await?
            .trim()
            .to_string();
        log(&format!(
            "reset --hard {} in {}",
            &sha[..sha.len().min(8)],
            task.path.display()
        ));
        git::reset_hard(&task.path, &sha).await?;
        reg.set_head(&task.slug, &review.head_sha)?;
        let text = format!(
            "Re-review {}: new commits since {} (now at {}). Diff only what changed, verify your own threads, then plan.",
            review.display_id,
            &old_sha[..old_sha.len().min(8)],
            &review.head_sha[..review.head_sha.len().min(8)]
        );
        // Deliver through the task's own adapter: find the live row, else build one from the
        // registry (port + the agent pane).
        let task_agent = engine.agent_adapter(&task.agent).with_context(|| {
            format!(
                "agent `{}` (which started this review) is not enabled",
                task.agent
            )
        })?;
        let row = engine
            .snapshot(false)
            .await
            .overview
            .iter()
            .flat_map(|r| r.agents.clone())
            .find(|a| a.cwd == crate::procs::canon(&task.path) && a.adapter == task.agent)
            .unwrap_or_else(|| crate::model::AgentRow {
                adapter: task.agent.clone(),
                pid: 0,
                cwd: task.path.clone(),
                session_id: None,
                title: None,
                mode: None,
                model: None,
                activity: crate::model::Activity::Idle,
                running_tool: None,
                plan_url: None,
                plan_title: None,
                port: task.port,
                last_activity: None,
                pane_target: Some(format!("{}:1.1", task.tmux_session)),
                pending: vec![],
                control_url: None,
                control_token: None,
            });
        log(&format!("send via {}", task.agent));
        task_agent.send(&row, &SendMsg::text(text)).await?;
        if opts.attach {
            tmux::attach(&task.tmux_session).await?;
        }
        return Ok(ReviewOutcome::ReReviewed {
            slug: task.slug,
            old_sha,
            new_sha: review.head_sha,
        });
    }

    let slug = format!("review-{id}");
    let path = repo.worktrees_dir().join(&slug);
    if path.exists() {
        bail!(
            "{} already exists but is not in the registry; remove it or run `fleet worktree gc`",
            path.display()
        );
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    log(&format!(
        "worktree add --detach {} FETCH_HEAD",
        path.display()
    ));
    git::worktree_add_detached(&repo.repo, &path, "FETCH_HEAD").await?;

    let port = free_port()?;
    reg.insert(&Task {
        slug: slug.clone(),
        kind: "review".into(),
        repo: repo.name.clone(),
        branch: None,
        path: path.clone(),
        old_paths: vec![],
        tmux_session: slug.clone(),
        port: Some(port),
        agent: agent_name.clone(),
        mode: Some(task.mode.clone()),
        task: Some(task_name.clone()),
        review_source: Some(source.clone()),
        review_id: Some(id),
        head_sha: Some(review.head_sha.clone()),
        first_prompt: None,
        created_at: Utc::now(),
        closed_at: None,
    })?;

    let mut skill_names = vec![];
    for sk in &task.skills {
        match agent.prepare_skill(sk) {
            Ok(n) => skill_names.push(n),
            Err(e) => log(&format!("skill {}: {e:#}; not loaded", sk.display())),
        }
    }
    let facts = super::prompt::Facts {
        slug: slug.clone(),
        repo: repo.name.clone(),
        branch: review.source_branch.clone(),
        base: review.target_branch.clone(),
        agent: agent_name.clone(),
        review_id: review.display_id.clone(),
        review_url: review.url.clone(),
        review_title: review.title.clone(),
        source_branch: review.source_branch.clone(),
        target_branch: review.target_branch.clone(),
        temp_slug: false,
    };
    let system = super::prompt::system(&task, &facts, &skill_names);
    let prompt = super::prompt::fill(task.prompt.as_deref().unwrap_or(""), &facts);
    let cmd = agent.launch(&LaunchSpec {
        cwd: path.clone(),
        mode: Mode::parse(&task.mode).unwrap_or(Mode::Plan),
        model: task.model.clone(),
        system,
        prompt,
        port,
    });
    log(&format!("tmux new-session -s {slug}"));
    tmux::new_session(&slug, &path).await?;
    // Shell on the left (pane 0), the agent on the right (pane 1).
    tmux::split_window(&format!("{slug}:1"), &path, None).await?;
    if let Some(c) = cfg.defaults.left_pane.as_deref().filter(|c| !c.is_empty()) {
        tmux::send_keys(&format!("{slug}:1.0"), c).await.ok();
    }
    let target = format!("{slug}:1.1");
    tmux::send_keys(&target, &cmd).await?;
    tmux::select_pane(&target).await.ok();
    if opts.attach {
        tmux::attach(&slug).await?;
    }
    Ok(ReviewOutcome::Started { slug, path, port })
}
