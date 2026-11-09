use chrono::{DateTime, Utc};
use fleet_core::model::{Activity, Overall, ReviewStatus};
use ratatui::style::{Color, Modifier, Style};

pub fn trunc(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else if n == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

pub fn ago(t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else { return "—".into() };
    let d = Utc::now().signed_duration_since(t);
    let s = d.num_seconds().max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// One meaning, one style, in every tab (spec: tui-machines, "One state vocabulary").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meaning {
    /// running / working `●`
    Running,
    /// in progress `◌`
    Progress,
    /// needs you `◆ ? !`
    NeedsYou,
    /// idle `○`
    Idle,
    /// paused `⏸`
    Paused,
    /// stopped / gone `■`
    Stopped,
    /// failed `✕`
    Failed,
    /// done / good `✓`
    Good,
    /// nothing to say
    Plain,
}

pub fn meaning_style(m: Meaning) -> Style {
    let s = Style::default();
    match m {
        Meaning::Running => s.fg(Color::Cyan),
        Meaning::Progress => s.fg(Color::Cyan).add_modifier(Modifier::DIM),
        Meaning::NeedsYou => s.fg(Color::Yellow).add_modifier(Modifier::BOLD),
        Meaning::Idle => s.fg(Color::DarkGray),
        Meaning::Paused => s.fg(Color::Blue),
        Meaning::Stopped => s.fg(Color::DarkGray).add_modifier(Modifier::DIM),
        Meaning::Failed => s.fg(Color::Red).add_modifier(Modifier::BOLD),
        Meaning::Good => s.fg(Color::Green).add_modifier(Modifier::BOLD),
        Meaning::Plain => s,
    }
}

pub fn overall_style(o: Overall) -> Style {
    meaning_style(match o {
        Overall::NeedsYou => Meaning::NeedsYou,
        Overall::Failed => Meaning::Failed,
        Overall::Ready | Overall::Merged => Meaning::Good,
        Overall::Working => Meaning::Running,
        Overall::Idle => Meaning::Idle,
        Overall::Dead => Meaning::Stopped,
    })
}

pub fn activity_meaning(a: Activity) -> Meaning {
    match a {
        Activity::WaitPlan | Activity::WaitQuestion | Activity::WaitPermission => Meaning::NeedsYou,
        Activity::Working => Meaning::Running,
        Activity::Idle => Meaning::Idle,
        Activity::None => Meaning::Stopped,
    }
}

pub fn activity_style(a: Activity) -> Style {
    meaning_style(activity_meaning(a))
}

pub fn review_style(s: ReviewStatus) -> Style {
    meaning_style(match s {
        ReviewStatus::Approved | ReviewStatus::Mergeable | ReviewStatus::Merged => Meaning::Good,
        ReviewStatus::CiFailed | ReviewStatus::Conflicts => Meaning::Failed,
        ReviewStatus::Draft => Meaning::Idle,
        ReviewStatus::Closed => Meaning::Stopped,
        ReviewStatus::Open => Meaning::Plain,
    })
}

/// A machine's state: glyph, label and meaning.
pub fn machine_state(s: &fleet_core::machines::MachineState) -> (&'static str, String, Meaning) {
    use fleet_core::machines::{MachineState as M, PausedBy};
    match s {
        M::Running => ("●", "running".into(), Meaning::Running),
        M::Starting => ("◌", "starting".into(), Meaning::Progress),
        M::Paused { by } => (
            "⏸",
            match by {
                PausedBy::Idle => "paused (idle)".into(),
                PausedBy::MaxRun => "paused (run cap)".into(),
                PausedBy::User => "paused".into(),
            },
            Meaning::Paused,
        ),
        M::Stopped => ("■", "stopped".into(), Meaning::Stopped),
        M::Cooldown { until } => (
            "⏸",
            format!(
                "until {}",
                until.with_timezone(&chrono::Local).format("%H:%M")
            ),
            Meaning::Paused,
        ),
        M::Error { .. } => ("✕", "error".into(), Meaning::Failed),
        M::Gone => ("■", "gone".into(), Meaning::Stopped),
    }
}

/// Type icons (Nerd Font, required; README › Install).
pub mod icon {
    /// nf-md-laptop
    pub const LAPTOP: &str = "\u{f0322}";
    /// nf-md-cloud_outline
    pub const CLOUD: &str = "\u{f0163}";
    /// nf-cod-terminal
    pub const SESSION: &str = "\u{ea85}";
    /// nf-dev-git_branch
    pub const BRANCH: &str = "\u{e725}";
    /// nf-oct-git_pull_request
    pub const REVIEW: &str = "\u{f407}";
    /// The icon of a machine on `provider`.
    pub fn machine(provider: &str) -> &'static str {
        if provider == "local" {
            LAPTOP
        } else {
            CLOUD
        }
    }
}

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn accent() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

pub fn fuzzy_match(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = hay.to_lowercase();
    let needle = needle.to_lowercase();
    // every whitespace-separated term must be a substring (simple, predictable)
    needle.split_whitespace().all(|t| hay.contains(t))
}

pub fn open_url(url: &str) -> anyhow::Result<()> {
    open::that_detached(url).map_err(|e| anyhow::anyhow!("open {url}: {e}"))
}

pub fn copy_to_clipboard(s: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut c = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    c.stdin.as_mut().unwrap().write_all(s.as_bytes())?;
    c.wait()?;
    Ok(())
}

/// Whether the `fleet` binary on disk is newer than the one this process was started from.
///
/// A running TUI keeps the code it was started with; `cargo install` swaps the file under
/// it, so an old TUI left in a tmux pane silently keeps old behaviour (an old `w D` that only
/// removed the worktree looked like a broken `w x`). `started` is the exe's mtime at startup.
pub fn newer_binary_installed(started: Option<std::time::SystemTime>) -> bool {
    let (Some(started), Ok(exe)) = (started, std::env::current_exe()) else {
        return false;
    };
    std::fs::metadata(exe)
        .and_then(|m| m.modified())
        .map(|now| now > started)
        .unwrap_or(false)
}

/// The exe's mtime right now, to remember at startup.
pub fn exe_mtime() -> Option<std::time::SystemTime> {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod stale_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn newer_only_when_disk_mtime_moved_forward() {
        // The test binary's own mtime is "now-ish": a start time far in the future is never stale,
        // a start time at the epoch always is, and None (unknown) is never stale.
        assert!(!newer_binary_installed(Some(
            SystemTime::now() + Duration::from_secs(3600)
        )));
        assert!(newer_binary_installed(Some(SystemTime::UNIX_EPOCH)));
        assert!(!newer_binary_installed(None));
    }
}
