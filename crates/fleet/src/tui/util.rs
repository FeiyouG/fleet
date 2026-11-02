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
