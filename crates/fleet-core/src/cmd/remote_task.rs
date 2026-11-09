//! Acting on a task that runs on a cloud machine: attach, send, stop, rename, close, plan,
//! diff. Everything goes through the machine (tmux and git there), the same commands fleet
//! runs locally.

use super::Progress;
use crate::config::{Config, MachineConfig};
use crate::machines::{adapter_for, remote, MachineAdapter};
use crate::registry::{Registry, Task};
use crate::slug;
use anyhow::{bail, Context, Result};

/// The machine adapter and profile that reach a remote task.
pub struct Reach {
    pub profile: MachineConfig,
    pub adapter: Box<dyn MachineAdapter>,
    pub task: Task,
}

impl Reach {
    pub fn on(&self) -> remote::On<'_> {
        remote::On {
            adapter: self.adapter.as_ref(),
            id: self.task.machine_id.clone(),
        }
    }
}

/// The open remote task named `name` (its slug), if there is one.
pub fn find(name: &str) -> Result<Option<Task>> {
    Ok(Registry::open()?.get(name)?.filter(|t| t.is_remote()))
}

/// The open remote task behind a row's pane target (`<session>:1.1`). Task names are unique
/// across machines, so the session identifies it.
pub fn find_by_pane(target: &str) -> Result<Option<Task>> {
    let session = target.split(':').next().unwrap_or(target);
    Ok(Registry::open()?
        .open_tasks()?
        .into_iter()
        .find(|t| t.is_remote() && t.tmux_session == session))
}

pub fn reach(cfg: &Config, task: Task) -> Result<Reach> {
    let profile = cfg.machine(&task.machine).with_context(|| {
        format!(
            "task `{}` runs on profile `{}`, which is no longer in config",
            task.slug, task.machine
        )
    })?;
    let adapter = adapter_for(&profile)?;
    Ok(Reach {
        profile,
        adapter,
        task,
    })
}

/// Attach to the task's tmux session (wakes the machine).
pub async fn attach(r: &Reach) -> Result<()> {
    super::start::attach_remote(&r.on(), &r.task.tmux_session).await
}

/// Type `text` into the agent pane. Per-message mode/model switches need the agent's local
/// control endpoint, which is inside the machine: refused, never silently dropped.
pub async fn send(r: &Reach, text: &str, mode: Option<&str>, model: Option<&str>) -> Result<()> {
    if mode.is_some() || model.is_some() {
        bail!(
            "a per-message mode/model is not supported for agents on cloud machines yet; send without --mode/--model"
        );
    }
    r.adapter.resume(&r.task.machine_id).await.ok();
    r.on()
        .send_keys(&format!("{}:1.1", r.task.tmux_session), text)
        .await
}

/// Kill the agent pane (the worktree and machine stay).
pub async fn stop(r: &Reach) -> Result<()> {
    r.adapter.resume(&r.task.machine_id).await.ok();
    r.on()
        .run(
            &[
                "tmux",
                "kill-pane",
                "-t",
                &format!("{}:1.1", r.task.tmux_session),
            ],
            None,
        )
        .await
        .map(|_| ())
}

/// A private link to the plan page (port `port` on the machine).
pub async fn plan_url(r: &Reach, port: u16) -> Result<String> {
    r.adapter.port_url(&r.task.machine_id, port).await
}

/// A new tmux window on the machine with nvim's diff against `origin/<base>`, then attach.
pub async fn diff(r: &Reach, base: &str) -> Result<()> {
    r.adapter.resume(&r.task.machine_id).await.ok();
    let path = r.task.path.to_string_lossy().into_owned();
    let cmd = format!("nvim -c 'DiffviewOpen origin/{base}' || git diff origin/{base} | less -R");
    r.on()
        .run(
            &[
                "tmux",
                "new-window",
                "-t",
                &r.task.tmux_session,
                "-n",
                "diff",
                "-c",
                &path,
                &cmd,
            ],
            None,
        )
        .await?;
    attach(r).await
}

/// What could be lost by closing: uncommitted files and commits not on any remote.
pub async fn unsaved(r: &Reach) -> Result<Vec<String>> {
    let on = r.on();
    let p = r.task.path.as_path();
    let mut out = vec![];
    let dirty = on.run(&["git", "status", "--porcelain"], Some(p)).await?;
    let n = dirty.lines().filter(|l| !l.trim().is_empty()).count();
    if n > 0 {
        out.push(format!("{n} uncommitted file(s)"));
    }
    let unpushed = on
        .exec(
            &["git", "log", "--oneline", "HEAD", "--not", "--remotes"],
            Some(p),
        )
        .await?;
    if unpushed.success() {
        let n = unpushed.stdout.lines().count();
        if n > 0 && r.task.kind == "start" {
            out.push(format!("{n} commit(s) on no remote (push first)"));
        }
    }
    Ok(out)
}

/// Close a remote task: refuse when work would be lost (unless `force`); a per-task machine
/// is deleted, on a shared one the session, worktree and local branch are removed.
pub async fn close(r: &Reach, force: bool, log: &Progress) -> Result<()> {
    let state = r.adapter.get(&r.task.machine_id).await;
    let reachable = match &state {
        Ok(m) => !matches!(m.state, crate::machines::MachineState::Gone),
        Err(_) => false,
    };
    if reachable && !force {
        r.adapter.resume(&r.task.machine_id).await.ok();
        let lost = unsaved(r).await?;
        if !lost.is_empty() {
            bail!(
                "{} on {}: {}; close --force to close anyway",
                r.task.slug,
                state.as_ref().map(|m| m.name.as_str()).unwrap_or("?"),
                lost.join(", ")
            );
        }
    }
    if reachable {
        if r.profile.per_task {
            log(&format!("delete machine {}", r.task.machine_id));
            r.adapter.destroy(&r.task.machine_id).await?;
        } else {
            let on = r.on();
            let layout = remote::Layout::new(&r.profile, &r.task.repo);
            log(&format!(
                "kill session {} on {}",
                r.task.tmux_session, on.id
            ));
            on.exec(
                &[
                    "tmux",
                    "kill-session",
                    "-t",
                    &format!("={}", r.task.tmux_session),
                ],
                None,
            )
            .await
            .ok();
            let p = r.task.path.to_string_lossy().into_owned();
            log(&format!("worktree remove {p}"));
            on.run(
                &["git", "worktree", "remove", "--force", &p],
                Some(&layout.repo),
            )
            .await?;
            if let Some(b) = &r.task.branch {
                on.exec(&["git", "branch", "-D", b], Some(&layout.repo))
                    .await
                    .ok();
            }
            // The last task on a shared machine takes the machine with it.
            let others = Registry::open()?
                .open_tasks()?
                .into_iter()
                .filter(|t| t.machine_id == r.task.machine_id && t.slug != r.task.slug)
                .count();
            if others == 0 {
                log(&format!("delete machine {} (last task)", r.task.machine_id));
                r.adapter.destroy(&r.task.machine_id).await?;
            }
        }
    }
    Registry::open()?.close(&r.task.slug)
}

/// Rename a remote task: branch (ours only), worktree folder (+ symlink at the old path),
/// tmux session, registry. The machine keeps its name.
pub async fn rename(r: &Reach, new: &str, log: &Progress) -> Result<super::rename::Renamed> {
    let on = r.on();
    r.adapter.resume(&r.task.machine_id).await.ok();
    let layout = remote::Layout::new(&r.profile, &r.task.repo);
    let new_path = layout.worktree(new);
    if on.has_session(new).await {
        bail!("tmux session `{new}` already exists on the machine");
    }
    let new_branch = match &r.task.branch {
        Some(b) if r.task.kind == "start" => {
            let nb = slug::branch_for(new);
            log(&format!("git branch -m {b} {nb}"));
            on.run(&["git", "branch", "-m", b, &nb], Some(&layout.repo))
                .await?;
            Some(nb)
        }
        other => other.clone(),
    };
    let from = r.task.path.to_string_lossy().into_owned();
    let to = new_path.to_string_lossy().into_owned();
    log(&format!("git worktree move {from} {to}"));
    on.run(&["git", "worktree", "move", &from, &to], Some(&layout.repo))
        .await?;
    on.run(&["ln", "-s", &to, &from], None).await?;
    if on.has_session(&r.task.tmux_session).await {
        on.run(
            &["tmux", "rename-session", "-t", &r.task.tmux_session, new],
            None,
        )
        .await?;
    }
    Registry::open()?.rename(&r.task.slug, new, new_branch.as_deref(), &new_path)?;
    Ok(super::rename::Renamed {
        slug: new.into(),
        branch: new_branch,
        path: new_path,
    })
}

/// Re-review on the machine: fetch the review head there, reset the worktree, tell the agent.
pub async fn rereview(r: &Reach, refspec: &str, text: &str, log: &Progress) -> Result<String> {
    let on = r.on();
    r.adapter.resume(&r.task.machine_id).await.ok();
    let layout = remote::Layout::new(&r.profile, &r.task.repo);
    log(&format!("fetch origin {refspec} on {}", on.id));
    on.fetch(&layout.repo, "origin", refspec).await?;
    let sha = on
        .run(&["git", "rev-parse", "FETCH_HEAD"], Some(&layout.repo))
        .await?
        .trim()
        .to_string();
    log(&format!(
        "reset --hard {} in {}",
        &sha[..sha.len().min(8)],
        r.task.path.display()
    ));
    on.run(&["git", "reset", "--hard", &sha], Some(&r.task.path))
        .await?;
    send(r, text, None, None).await?;
    Ok(sha)
}
