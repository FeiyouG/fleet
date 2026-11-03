//! `fleet agent-hook <agent>`: the Claude Code / Codex hook handler. See `agents::hooks`.

use crate::agents::hooks;
use anyhow::Result;

pub fn run(agent: &str) -> Result<()> {
    // The hook runs as `sh -c "fleet agent-hook claude"` (shell form) or directly (exec
    // form). The agent process is the nearest ancestor that is not a shell.
    let pid = agent_pid();
    hooks::run_from_stdin(agent, pid)
}

#[cfg(unix)]
fn agent_pid() -> u32 {
    let procs = crate::procs::ProcTable::snapshot();
    let mut cur = std::os::unix::process::parent_id();
    for _ in 0..8 {
        match procs.get(cur) {
            Some(p) if matches!(p.name.as_str(), "sh" | "bash" | "zsh" | "dash" | "fish") => {
                match p.ppid {
                    Some(pp) => cur = pp,
                    None => break,
                }
            }
            _ => break,
        }
    }
    cur
}

#[cfg(not(unix))]
fn agent_pid() -> u32 {
    0
}
