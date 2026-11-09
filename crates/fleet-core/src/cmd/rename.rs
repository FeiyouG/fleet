//! `fleet agent rename OLD NEW`: branch + worktree folder (+ symlink at the old path, because
//! the running agent keeps absolute paths) + tmux session + registry.

use super::Progress;
use crate::config::Config;
use crate::registry::Registry;
use crate::slug;
use crate::tmux;
use crate::worktrees::git;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Renamed {
    pub slug: String,
    pub branch: Option<String>,
    pub path: PathBuf,
}

pub async fn rename(cfg: &Config, old: &str, new: &str, log: &Progress) -> Result<Renamed> {
    if !slug::is_valid(new) {
        bail!("`{new}` is not a valid name (lowercase, digits, -, _, .)");
    }
    if old == new {
        bail!("same name");
    }
    let reg = Registry::open()?;
    let task = reg
        .get(old)?
        .with_context(|| format!("`{old}` is not a fleet-started task (see `fleet agent list`)"))?;
    if reg.get(new)?.is_some() {
        bail!("`{new}` is already an open task");
    }
    if task.is_remote() {
        let r = super::remote_task::reach(cfg, task)?;
        return super::remote_task::rename(&r, new, log).await;
    }
    let repo = cfg
        .project(&task.repo)
        .with_context(|| format!("repo `{}` not in config", task.repo))?;
    let new_path = repo.worktrees_dir().join(new);
    if new_path.exists() {
        bail!("{} already exists", new_path.display());
    }
    if tmux::has_session(new).await {
        bail!("tmux session `{new}` already exists");
    }

    // Branch: only rename ours (wip/* or a branch derived from the old slug).
    let new_branch = match &task.branch {
        Some(b) if task.kind == "start" => {
            let nb = slug::branch_for(new);
            log(&format!("git branch -m {b} {nb}"));
            git::branch_rename(&repo.repo, b, &nb).await?;
            Some(nb)
        }
        other => other.clone(),
    };

    log(&format!(
        "git worktree move {} {}",
        task.path.display(),
        new_path.display()
    ));
    git::worktree_move(&repo.repo, &task.path, &new_path).await?;
    // Keep the old absolute path valid for the running agent.
    std::os::unix::fs::symlink(&new_path, &task.path)
        .with_context(|| format!("symlink {} -> {}", task.path.display(), new_path.display()))?;
    log(&format!(
        "symlink {} -> {}",
        task.path.display(),
        new_path.display()
    ));

    if tmux::has_session(&task.tmux_session).await {
        log(&format!("tmux rename-session {} {new}", task.tmux_session));
        tmux::rename_session(&task.tmux_session, new).await?;
    }
    reg.rename(old, new, new_branch.as_deref(), &new_path)?;
    Ok(Renamed {
        slug: new.into(),
        branch: new_branch,
        path: new_path,
    })
}
