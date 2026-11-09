//! tmux: list panes, attach, send keys, create sessions.

use crate::model::PaneRow;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const SEP: &str = "\u{1f}";

pub async fn list_panes() -> Result<Vec<PaneRow>> {
    let fmt = [
        "#{pane_pid}",
        "#{session_name}",
        "#{window_index}",
        "#{window_name}",
        "#{pane_index}",
        "#{pane_current_path}",
        "#{pane_current_command}",
        "#{pane_title}",
        "#{session_attached}",
    ]
    .join(SEP);
    let out = Command::new("tmux")
        .args(["list-panes", "-a", "-F", &fmt])
        .output()
        .await;
    let out = match out {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("running tmux"),
    };
    if !out.status.success() {
        // No server running is not an error: there are simply no panes.
        return Ok(vec![]);
    }
    Ok(parse_panes(&String::from_utf8_lossy(&out.stdout)))
}

pub fn parse_panes(s: &str) -> Vec<PaneRow> {
    s.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(SEP).collect();
            if f.len() < 9 {
                return None;
            }
            Some(PaneRow {
                machine: None,
                pid: f[0].parse().ok()?,
                session: f[1].to_string(),
                window: f[2].parse().ok()?,
                window_name: f[3].to_string(),
                pane: f[4].parse().ok()?,
                cwd: PathBuf::from(f[5]),
                command: f[6].to_string(),
                title: f[7].to_string(),
                attached: f[8].parse::<u32>().map(|n| n > 0).unwrap_or(false),
            })
        })
        .collect()
}

pub fn inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

async fn run(args: &[&str]) -> Result<String> {
    let out = Command::new("tmux")
        .args(args)
        .output()
        .await
        .context("running tmux")?;
    if !out.status.success() {
        bail!(
            "tmux {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn has_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", &format!("={name}")])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The unambiguous form of a session name for `-t`: `=name:`. `=` disables prefix matching and
/// the trailing `:` stops tmux at the session step. A bare name falls through to windows and
/// then to panes, where it matches the *command running in the pane*, so a session called
/// `fleet` (or `nvim`, `opencode`) would resolve to some unrelated pane running that program.
pub fn session_target(name: &str) -> String {
    format!("={name}:")
}

/// Switch (inside tmux) or attach (outside) to a target like `sess`, `sess:1` or `sess:1.2`.
/// Outside tmux this execs `tmux attach` in the foreground and returns when it detaches.
pub async fn attach(target: &str) -> Result<()> {
    let session = target.split(':').next().unwrap_or(target);
    if inside_tmux() {
        run(&["switch-client", "-t", &session_target(session)]).await?;
        if target.contains(':') {
            run(&["select-window", "-t", target]).await.ok();
            if target.contains('.') {
                run(&["select-pane", "-t", target]).await.ok();
            }
        }
        Ok(())
    } else {
        let st = std::process::Command::new("tmux")
            .args(["attach", "-t", &session_target(session)])
            .status()
            .context("tmux attach")?;
        if !st.success() {
            bail!("tmux attach -t {session} failed");
        }
        Ok(())
    }
}

pub async fn new_session(name: &str, cwd: &Path) -> Result<()> {
    run(&[
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        &cwd.to_string_lossy(),
    ])
    .await
    .map(|_| ())
}

pub async fn split_window(target: &str, cwd: &Path, cmd: Option<&str>) -> Result<()> {
    let cwd = cwd.to_string_lossy().into_owned();
    let mut args = vec!["split-window", "-h", "-t", target, "-c", &cwd];
    if let Some(c) = cmd {
        args.push(c);
    }
    run(&args).await.map(|_| ())
}

pub async fn select_pane(target: &str) -> Result<()> {
    run(&["select-pane", "-t", target]).await.map(|_| ())
}

/// Type `text` into a pane and press Enter.
pub async fn send_keys(target: &str, text: &str) -> Result<()> {
    run(&["send-keys", "-t", target, "-l", text]).await?;
    run(&["send-keys", "-t", target, "Enter"]).await.map(|_| ())
}

pub async fn rename_session(old: &str, new: &str) -> Result<()> {
    run(&["rename-session", "-t", &session_target(old), new])
        .await
        .map(|_| ())
}

/// Kill a session. Already gone (or no tmux server at all) counts as success.
pub async fn kill_session(name: &str) -> Result<()> {
    gone_ok(run(&["kill-session", "-t", &session_target(name)]).await)
}

/// Kill a pane. Already gone counts as success.
pub async fn kill_pane(target: &str) -> Result<()> {
    gone_ok(run(&["kill-pane", "-t", target]).await)
}

fn gone_ok(r: Result<String>) -> Result<()> {
    match r {
        Ok(_) => Ok(()),
        Err(e) if is_gone(&format!("{e:#}")) => Ok(()),
        Err(e) => Err(e),
    }
}

/// tmux's wording for "that target does not exist".
pub fn is_gone(msg: &str) -> bool {
    [
        "can't find",
        "no server running",
        "error connecting to",
        "not found",
    ]
    .iter()
    .any(|m| msg.contains(m))
}

/// Open a new window in `session` at `cwd` (used for "shell here").
pub async fn new_window(session: &str, cwd: &Path) -> Result<()> {
    run(&[
        "new-window",
        "-t",
        &session_target(session),
        "-c",
        &cwd.to_string_lossy(),
    ])
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture() {
        let s = include_str!("../../../fixtures/tmux-panes.txt");
        let panes = parse_panes(s);
        assert!(panes.len() >= 3, "{}", panes.len());
        let oc: Vec<_> = panes.iter().filter(|p| p.command == "opencode").collect();
        assert!(!oc.is_empty());
        assert_eq!(
            panes[0].target(),
            format!("{}:{}.{}", panes[0].session, panes[0].window, panes[0].pane)
        );
    }

    #[test]
    fn target_split() {
        assert_eq!(split_target("s"), ("s".into(), None, None));
        assert_eq!(split_target("=s:"), ("s".into(), None, None));
        assert_eq!(split_target("s:2"), ("s".into(), Some("2".into()), None));
        assert_eq!(
            split_target("fix-x:1.1"),
            ("fix-x".into(), Some("1".into()), Some("1".into()))
        );
    }

    #[test]
    fn session_target_is_exact_and_session_only() {
        assert_eq!(session_target("fleet"), "=fleet:");
        assert_eq!(session_target("wip-x"), "=wip-x:");
    }

    #[test]
    fn gone_messages() {
        assert!(is_gone("tmux kill-session -t =x: can't find session: x"));
        assert!(is_gone("no server running on /tmp/tmux-501/default"));
        assert!(!is_gone("permission denied"));
    }
}

/// `sess`, `sess:1` or `sess:1.2` -> (session, window, pane).
pub fn split_target(target: &str) -> (String, Option<String>, Option<String>) {
    let (sess, rest) = match target.split_once(':') {
        Some((s, r)) => (s, Some(r)),
        None => (target, None),
    };
    let sess = sess.trim_start_matches('=').to_string();
    match rest {
        None | Some("") => (sess, None, None),
        Some(r) => match r.split_once('.') {
            Some((w, p)) => (sess, Some(w.to_string()), Some(p.to_string())),
            None => (sess, Some(r.to_string()), None),
        },
    }
}

/// Bring a tmux target in front of the user from OUTSIDE any pane (a notification click, a
/// shell): every attached client is switched to the session, window and pane, and the terminal
/// application that hosts the first client is activated (brought to the front) on macOS.
/// Nothing to do without an attached client: the session is left as it is.
pub async fn focus(target: &str) -> Result<()> {
    let (sess, win, pane) = split_target(target);
    let clients = run(&["list-clients", "-F", "#{client_tty}\t#{client_pid}"]).await?;
    let mut first_pid: Option<u32> = None;
    let mut any = false;
    for line in clients.lines() {
        let Some((tty, pid)) = line.split_once('\t') else {
            continue;
        };
        any = true;
        if first_pid.is_none() {
            first_pid = pid.trim().parse().ok();
        }
        run(&["switch-client", "-c", tty, "-t", &session_target(&sess)]).await?;
        if let Some(w) = &win {
            let wt = format!("{}:{w}", session_target(&sess).trim_end_matches(':'));
            run(&["select-window", "-t", &wt]).await.ok();
            if let Some(p) = &pane {
                run(&["select-pane", "-t", &format!("{wt}.{p}")]).await.ok();
            }
        }
    }
    if !any {
        bail!("no tmux client is attached; nothing to focus");
    }
    if let Some(app) = first_pid.and_then(terminal_app_of) {
        activate_app(&app).await;
    }
    Ok(())
}

/// The macOS application that hosts the tmux client `pid`: walk the parents until a process
/// name that looks like a terminal (`WezTerm`, `iTerm2`, `Terminal`, `Ghostty`, `kitty`,
/// `Alacritty`, `Warp`, `Hyper`). `None` when unknown.
pub fn terminal_app_of(client_pid: u32) -> Option<String> {
    let table = crate::procs::ProcTable::snapshot();
    for pid in table.ancestors(client_pid) {
        let Some(name) = table.get(pid).map(|p| p.name.to_ascii_lowercase()) else {
            continue;
        };
        let app = match name.as_str() {
            n if n.contains("wezterm") => "WezTerm",
            n if n.contains("iterm") => "iTerm2",
            n if n.contains("ghostty") => "Ghostty",
            n if n.contains("kitty") => "kitty",
            n if n.contains("alacritty") => "Alacritty",
            n if n.contains("warp") => "Warp",
            n if n.contains("hyper") => "Hyper",
            "terminal" => "Terminal",
            _ => continue,
        };
        return Some(app.to_string());
    }
    None
}

/// `osascript -e 'tell application "X" to activate'`; silent when it fails (not macOS, app gone).
pub async fn activate_app(app: &str) {
    let _ = tokio::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"{app}\" to activate")])
        .output()
        .await;
}
