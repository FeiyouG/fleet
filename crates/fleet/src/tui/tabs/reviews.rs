use super::render_table;
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, activity_style, dim, review_style, trunc};
use fleet_core::model::{ReviewStatus, ReviewView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

pub const PRESETS: &[&str] = &["all", "mine", "others", "needs-me", "failed"];

fn me() -> String {
    // glab's identity; cached once per process.
    static ME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        std::process::Command::new("glab")
            .args(["api", "user"])
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
            .and_then(|v| v.get("username").and_then(|u| u.as_str()).map(String::from))
            .unwrap_or_default()
    })
    .clone()
}

pub fn visible(app: &App) -> Vec<&ReviewView> {
    let st = &app.tabs.reviews;
    let me = me();
    let preset = PRESETS[st.preset];
    let mut rows: Vec<&ReviewView> = app
        .fleet
        .reviews
        .iter()
        .filter(|r| {
            app.tabs
                .source
                .as_deref()
                .map(|s| r.review.source == s)
                .unwrap_or(true)
        })
        .filter(|r| match preset {
            "mine" => r.review.author == me,
            "others" => r.review.author != me,
            "needs-me" => {
                r.review.author != me
                    && !r.review.approvals.contains(&me)
                    && !r.review.status.is_done()
            }
            "failed" => matches!(
                r.review.status,
                ReviewStatus::CiFailed | ReviewStatus::Conflicts
            ),
            _ => true,
        })
        .filter(|r| {
            let hay = format!(
                "{} {} {} {} {} {}",
                r.review.display_id,
                r.review.title,
                r.review.author,
                r.review.source_branch,
                r.review.status.label(),
                r.worktrees
                    .iter()
                    .map(|w| w.name.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            util::fuzzy_match(&hay, &st.filter)
        })
        .collect();
    match app.sort {
        SortMode::Severity => rows.sort_by(|a, b| {
            let score = |r: &ReviewView| match r.review.status {
                ReviewStatus::CiFailed | ReviewStatus::Conflicts => 0,
                ReviewStatus::Open => 1,
                ReviewStatus::Approved | ReviewStatus::Mergeable => 2,
                ReviewStatus::Draft => 3,
                _ => 4,
            };
            score(a)
                .cmp(&score(b))
                .then(b.review.updated_at.cmp(&a.review.updated_at))
        }),
        SortMode::Activity => rows.sort_by(|a, b| b.review.updated_at.cmp(&a.review.updated_at)),
        SortMode::Name => rows.sort_by(|a, b| b.review.id.cmp(&a.review.id)),
    }
    rows
}

pub fn selected(app: &App) -> Option<&ReviewView> {
    visible(app).get(app.tabs.reviews.selected).copied()
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let rows_v = visible(app);
    let me = me();
    let rows: Vec<Row> = rows_v
        .iter()
        .map(|v| {
            let r = &v.review;
            let comments = if r.threads.total == 0 {
                Line::from(Span::styled("0", dim()))
            } else {
                Line::from(vec![
                    Span::raw(format!("{} ", r.threads.total)),
                    Span::styled(
                        format!("✔{}", r.threads.resolved),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("✘{}", r.threads.unresolved),
                        if r.threads.unresolved > 0 {
                            Style::default().fg(Color::Yellow)
                        } else {
                            dim()
                        },
                    ),
                ])
            };
            let wts = match v.worktrees.len() {
                0 => Line::from(Span::styled("0", dim())),
                n => Line::from(vec![
                    Span::raw(format!("{n} · ")),
                    Span::raw(trunc(&v.worktrees[0].name, 18)),
                    Span::styled(
                        if n > 1 {
                            format!(" +{}", n - 1)
                        } else {
                            String::new()
                        },
                        dim(),
                    ),
                ]),
            };
            let ags = match v.agents.first() {
                None => Line::from(Span::styled("0", dim())),
                Some(a) => Line::from(vec![
                    Span::raw(format!("{} · ", v.agents.len())),
                    Span::styled(format!("{} ", a.mode.as_deref().unwrap_or("?")), dim()),
                    Span::styled(
                        format!("{} {}", a.activity.glyph(), a.activity.label()),
                        activity_style(a.activity),
                    ),
                    Span::styled(
                        if v.agents.len() > 1 {
                            format!(" +{}", v.agents.len() - 1)
                        } else {
                            String::new()
                        },
                        dim(),
                    ),
                ]),
            };
            let author = if r.author == me {
                "me".to_string()
            } else {
                trunc(&r.author, 10)
            };
            Row::new(vec![
                Cell::from(r.display_id.clone()),
                Cell::from(trunc(&r.title, 40)),
                Cell::from(Span::styled(author, dim())),
                Cell::from(Span::styled(
                    r.status.label().to_string(),
                    review_style(r.status),
                )),
                Cell::from(r.ci.glyph()),
                Cell::from(format!("{}", r.approvals.len())),
                Cell::from(comments),
                Cell::from(wts),
                Cell::from(ags),
            ])
        })
        .collect();
    render_table(
        f,
        area,
        vec![
            "ID",
            "TITLE",
            "AUTHOR",
            "STATUS",
            "CI",
            "👍",
            "COMMENTS",
            "WORKTREES",
            "AGENTS",
        ],
        vec![
            Constraint::Length(6),
            Constraint::Length(40),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(12),
            Constraint::Length(26),
            Constraint::Min(22),
        ],
        rows,
        app.tabs.reviews.selected,
    );
}

pub fn detail(app: &App) -> Text<'static> {
    let Some(v) = selected(app) else {
        return Text::from("");
    };
    let r = &v.review;
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} ", r.display_id), util::accent()),
        Span::raw(format!(
            "{} · by {} · {} → {} · updated {} ago",
            trunc(&r.title, 80),
            r.author,
            r.source_branch,
            r.target_branch,
            util::ago(r.updated_at)
        )),
    ])];
    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(r.status.label().to_string(), review_style(r.status)),
        Span::raw(format!(
            " · CI {} · approved by {} · {} comments: {} resolved, {} open · ",
            r.ci.glyph(),
            if r.approvals.is_empty() {
                "nobody".into()
            } else {
                r.approvals.join(", ")
            },
            r.threads.total,
            r.threads.resolved,
            r.threads.unresolved
        )),
        Span::styled(r.url.clone(), dim()),
    ]));
    if v.worktrees.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no local worktree · v r (or Enter) starts a review agent",
            dim(),
        )));
    }
    for w in &v.worktrees {
        let ags: Vec<String> = v
            .agents
            .iter()
            .filter(|a| a.cwd.starts_with(&w.path))
            .map(|a| {
                format!(
                    "{} {} {}",
                    a.mode.as_deref().unwrap_or("?"),
                    a.activity.glyph(),
                    a.activity.label()
                )
            })
            .collect();
        lines.push(Line::from(format!(
            "   worktree {} ({} {}{}) · agents: {}",
            w.name,
            w.branch.as_deref().unwrap_or("detached"),
            w.sync_glyph(),
            if w.dirty { " *" } else { "" },
            if ags.is_empty() {
                "none".into()
            } else {
                ags.join(", ")
            }
        )));
    }
    Text::from(lines)
}
