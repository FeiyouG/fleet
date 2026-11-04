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

pub fn overall_style(o: Overall) -> Style {
    match o {
        Overall::NeedsYou => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Overall::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Overall::Ready => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        Overall::Merged => Style::default().fg(Color::Blue),
        Overall::Working => Style::default(),
        Overall::Idle => Style::default().fg(Color::DarkGray),
        Overall::Dead => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}

pub fn activity_style(a: Activity) -> Style {
    match a {
        Activity::WaitPlan | Activity::WaitQuestion | Activity::WaitPermission => {
            Style::default().fg(Color::Yellow)
        }
        Activity::Working => Style::default().fg(Color::Cyan),
        Activity::Idle => Style::default().fg(Color::DarkGray),
        Activity::None => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}

pub fn review_style(s: ReviewStatus) -> Style {
    match s {
        ReviewStatus::Approved | ReviewStatus::Mergeable => Style::default().fg(Color::Green),
        ReviewStatus::CiFailed | ReviewStatus::Conflicts => Style::default().fg(Color::Red),
        ReviewStatus::Draft => Style::default().fg(Color::DarkGray),
        ReviewStatus::Merged => Style::default().fg(Color::Blue),
        ReviewStatus::Closed => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
        ReviewStatus::Open => Style::default(),
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
