use super::render_table;
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, activity_style, dim, review_style, trunc};
use fleet_core::model::{GcVerdict, WorktreeView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

pub fn visible(app: &App) -> Vec<&WorktreeView> {
    let st = &app.tabs.worktrees;
    let mut rows: Vec<&WorktreeView> = app
        .fleet
        .worktrees
        .iter()
        .filter(|w| {
            let hay = format!(
                "{} {} {} {} {} {}",
                w.worktree.name,
                w.worktree.repo,
                w.worktree.branch.clone().unwrap_or_default(),
                w.worktree.path.display(),
                w.review
                    .as_ref()
                    .map(|r| format!("{} {}", r.display_id, r.status.label()))
                    .unwrap_or_default(),
                w.gc.label()
            );
            util::fuzzy_match(&hay, &st.filter)
        })
        .collect();
    match app.sort {
        SortMode::Name => rows.sort_by(|a, b| a.worktree.name.cmp(&b.worktree.name)),
        SortMode::Activity => rows.sort_by(|a, b| {
            b.agents
                .iter()
                .filter_map(|x| x.last_activity)
                .max()
                .cmp(&a.agents.iter().filter_map(|x| x.last_activity).max())
        }),
        SortMode::Severity => rows.sort_by(|a, b| {
            // gc candidates last, dirty/unsynced first
            let score = |w: &WorktreeView| match &w.gc {
                GcVerdict::Yes(_) => 3,
                GcVerdict::No(r) if r == "agent alive" => 0,
                GcVerdict::No(r) if r == "dirty" || r == "unpushed" => 1,
                _ => 2,
            };
            score(a)
                .cmp(&score(b))
                .then(a.worktree.name.cmp(&b.worktree.name))
        }),
    }
    rows
}

pub fn selected(app: &App) -> Option<&WorktreeView> {
    visible(app).get(app.tabs.worktrees.selected).copied()
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let rows_v = visible(app);
    let show_paths = app.tabs.show_paths;
    let home = fleet_core::config::home().to_string_lossy().into_owned();
    let rows: Vec<Row> = rows_v
        .iter()
        .map(|v| {
            let w = &v.worktree;
            let name = if show_paths {
                w.path.to_string_lossy().replace(&home, "~")
            } else {
                w.name.clone()
            };
            let agents = match v.agents.first() {
                None => Line::from(Span::styled("0", dim())),
                Some(a) => Line::from(vec![
                    Span::raw(format!("{} · ", v.agents.len())),
                    Span::styled(format!("{} ", a.mode.as_deref().unwrap_or("?")), dim()),
                    Span::styled(
                        format!("{} {}", a.activity.glyph(), a.activity.label()),
                        activity_style(a.activity),
                    ),
                ]),
            };
            let review = match &v.review {
                None => Line::from(Span::styled("—", dim())),
                Some(r) => Line::from(vec![
                    Span::raw(format!("{} ", r.display_id)),
                    Span::styled(r.status.label().to_string(), review_style(r.status)),
                ]),
            };
            let gc = match &v.gc {
                GcVerdict::Yes(r) => {
                    Span::styled(format!("yes: {r}"), Style::default().fg(Color::Yellow))
                }
                GcVerdict::No(r) => Span::styled(format!("no: {r}"), dim()),
            };
            Row::new(vec![
                Cell::from(trunc(&name, if show_paths { 56 } else { 30 })),
                Cell::from(Span::styled(trunc(&w.repo, 16), dim())),
                Cell::from(trunc(w.branch.as_deref().unwrap_or("(detached)"), 32)),
                Cell::from(w.sync_glyph()),
                Cell::from(if w.dirty { "*" } else { "" }),
                Cell::from(agents),
                Cell::from(review),
                Cell::from(gc),
            ])
        })
        .collect();
    render_table(
        f,
        area,
        vec![
            if show_paths { "PATH" } else { "NAME" },
            "REPO",
            "BRANCH",
            "SYNC",
            "DIRTY",
            "AGENTS",
            "REVIEW",
            "GC?",
        ],
        vec![
            Constraint::Length(if show_paths { 56 } else { 30 }),
            Constraint::Length(16),
            Constraint::Length(32),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(24),
            Constraint::Length(18),
            Constraint::Min(16),
        ],
        rows,
        app.tabs.worktrees.selected,
    );
}

pub fn detail(app: &App) -> Text<'static> {
    let Some(v) = selected(app) else {
        return Text::from("");
    };
    let w = &v.worktree;
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} ", w.name), util::accent()),
        Span::raw(format!(
            "{} · HEAD {} · upstream {} · ahead {} / behind {}{}",
            w.path.display(),
            w.head,
            w.upstream.as_deref().unwrap_or("none"),
            w.ahead,
            w.behind,
            if w.dirty { " · dirty" } else { " · clean" }
        )),
    ])];
    let sessions: std::collections::BTreeSet<_> =
        v.panes.iter().map(|p| p.session.clone()).collect();
    lines.push(Line::from(Span::styled(
        format!(
            "   tmux: {} · gc: {}",
            if sessions.is_empty() {
                "none".into()
            } else {
                sessions.into_iter().collect::<Vec<_>>().join(", ")
            },
            v.gc.label()
        ),
        dim(),
    )));
    for a in &v.agents {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{} {}", a.activity.glyph(), a.activity.label()),
                activity_style(a.activity),
            ),
            Span::raw(format!(
                " {} · {} · {} ago",
                a.mode.as_deref().unwrap_or("?"),
                trunc(a.title.as_deref().unwrap_or(""), 60),
                util::ago(a.last_activity)
            )),
        ]));
    }
    if let Some(r) = &v.review {
        lines.push(Line::from(vec![
            Span::raw(format!("   {} ", r.display_id)),
            Span::styled(r.status.label().to_string(), review_style(r.status)),
            Span::styled(format!(" · {}", r.url), dim()),
        ]));
    }
    Text::from(lines)
}
