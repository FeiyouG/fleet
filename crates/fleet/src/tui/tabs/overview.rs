use super::render_table;
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, activity_style, dim, overall_style, review_style, trunc};
use fleet_core::model::{AgentRow, OverviewRow, ReviewRow};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

pub fn visible(app: &App) -> Vec<&OverviewRow> {
    let st = &app.tabs.overview;
    let mut rows: Vec<&OverviewRow> = app
        .fleet
        .overview
        .iter()
        .filter(|r| {
            let hay = format!(
                "{} {} {} {} {} {}",
                r.name(),
                r.repo().unwrap_or(""),
                r.worktree
                    .as_ref()
                    .and_then(|w| w.branch.clone())
                    .unwrap_or_default(),
                r.agents
                    .iter()
                    .map(|a| format!(
                        "{} {}",
                        a.title.clone().unwrap_or_default(),
                        a.activity.label()
                    ))
                    .collect::<Vec<_>>()
                    .join(" "),
                r.panes
                    .iter()
                    .map(|p| p.session.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
                r.reviews
                    .iter()
                    .map(|x| format!("{} {}", x.display_id, x.status.label()))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            util::fuzzy_match(&hay, &st.filter)
        })
        .collect();
    match app.sort {
        SortMode::Severity => {}
        SortMode::Activity => rows.sort_by(|a, b| b.last_activity.cmp(&a.last_activity)),
        SortMode::Name => rows.sort_by_key(|r| r.name()),
    }
    rows
}

pub fn selected(app: &App) -> Option<&OverviewRow> {
    visible(app).get(app.tabs.overview.selected).copied()
}

pub fn agent_cell(a: &AgentRow) -> Cell<'static> {
    Cell::from(Line::from(vec![
        Span::styled(format!("{:<5} ", a.mode.as_deref().unwrap_or("?")), dim()),
        Span::styled(
            format!("{} {}", a.activity.glyph(), a.activity.label()),
            activity_style(a.activity),
        ),
    ]))
}

pub fn review_cell(r: Option<&ReviewRow>) -> Cell<'static> {
    match r {
        None => Cell::from(Span::styled("—", dim())),
        Some(r) => Cell::from(Line::from(vec![
            Span::raw(format!("{} ", r.display_id)),
            Span::styled(r.status.label().to_string(), review_style(r.status)),
            Span::styled(format!(" {} 👍{}", r.ci.glyph(), r.approvals.len()), dim()),
            Span::styled(
                if r.threads.unresolved > 0 {
                    format!(" 💬{} ✘{}", r.threads.total, r.threads.unresolved)
                } else {
                    format!(" 💬{}", r.threads.total)
                },
                if r.threads.unresolved > 0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    dim()
                },
            ),
        ])),
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let rows_v = visible(app);
    let show_paths = app.tabs.show_paths;
    let one_repo = app.cfg.repos.len() <= 1;
    let name_w = if show_paths { 44 } else { 26 };
    let mut rows = vec![];
    let mut outside_marker_done = false;
    for r in &rows_v {
        if r.worktree.is_none() && !outside_marker_done {
            outside_marker_done = true;
        }
        let w = r.worktree.as_ref();
        let name = if show_paths {
            r.dir
                .to_string_lossy()
                .replace(&*fleet_core::config::home().to_string_lossy(), "~")
        } else {
            r.name()
        };
        let branch = match w {
            Some(w) => match &w.branch {
                Some(b) => Line::from(vec![
                    Span::raw(trunc(b, 26)),
                    Span::styled(
                        format!(" {}{}", w.sync_glyph(), if w.dirty { " *" } else { "" }),
                        dim(),
                    ),
                ]),
                None => Line::from(Span::styled(
                    format!("(detached){}", if w.dirty { " *" } else { "" }),
                    dim(),
                )),
            },
            None => Line::from(Span::styled("—  outside repos", dim())),
        };
        let first = r.agents.first();
        let agent = match first {
            Some(a) => {
                let mut c = agent_cell(a);
                if r.agents.len() > 1 {
                    c = Cell::from(Line::from(vec![
                        Span::styled(format!("{:<5} ", a.mode.as_deref().unwrap_or("?")), dim()),
                        Span::styled(
                            format!("{} {}", a.activity.glyph(), a.activity.label()),
                            activity_style(a.activity),
                        ),
                        Span::styled(format!(" +{}", r.agents.len() - 1), dim()),
                    ]));
                }
                c
            }
            None => Cell::from(Span::styled("—     NONE", dim())),
        };
        let pane = r
            .primary_pane()
            .map(|p| {
                Line::from(vec![
                    Span::raw(trunc(&p.target(), 24)),
                    Span::styled(
                        if p.attached { " ●" } else { " ○" },
                        if p.attached {
                            Style::default().fg(Color::Green)
                        } else {
                            dim()
                        },
                    ),
                ])
            })
            .unwrap_or_else(|| Line::from(Span::styled("—", dim())));
        let review = r
            .reviews
            .iter()
            .find(|x| !x.status.is_done())
            .or(r.reviews.first());
        let mut cells = vec![
            Cell::from(Span::styled(
                r.overall.label().to_string(),
                overall_style(r.overall),
            )),
            Cell::from(trunc(&name, name_w)),
        ];
        if !one_repo {
            cells.push(Cell::from(Span::styled(
                trunc(r.repo().unwrap_or("—"), 14),
                dim(),
            )));
        }
        cells.extend([
            Cell::from(branch),
            agent,
            Cell::from(pane),
            review_cell(review),
        ]);
        rows.push(Row::new(cells));
        if app.tabs.overview.expanded {
            for a in r.agents.iter().skip(1) {
                let mut cells = vec![Cell::from(""), Cell::from(Span::styled("  ↳", dim()))];
                if !one_repo {
                    cells.push(Cell::from(""));
                }
                cells.extend([
                    Cell::from(""),
                    agent_cell(a),
                    Cell::from(Span::styled(
                        trunc(a.title.as_deref().unwrap_or(""), 26),
                        dim(),
                    )),
                    Cell::from(""),
                ]);
                rows.push(Row::new(cells));
            }
        }
    }
    let mut header = vec!["OVERALL", "NAME"];
    if !one_repo {
        header.push("REPO");
    }
    header.extend(["BRANCH", "AGENT", "TMUX", "REVIEW"]);
    let mut widths = vec![Constraint::Length(10), Constraint::Length(name_w as u16)];
    if !one_repo {
        widths.push(Constraint::Length(14));
    }
    widths.extend([
        Constraint::Length(34),
        Constraint::Length(22),
        Constraint::Length(27),
        Constraint::Min(24),
    ]);
    // Map selected logical row to physical row when expanded.
    let sel = if app.tabs.overview.expanded {
        rows_v
            .iter()
            .take(app.tabs.overview.selected)
            .map(|r| 1 + r.agents.len().saturating_sub(1))
            .sum()
    } else {
        app.tabs.overview.selected
    };
    render_table(f, area, header, widths, rows, sel);
}

pub fn detail(app: &App) -> Text<'static> {
    let Some(r) = selected(app) else {
        return Text::from("");
    };
    let mut lines = vec![];
    let mut l1 = vec![Span::styled(format!(" {} ", r.name()), util::accent())];
    if let Some(w) = &r.worktree {
        l1.push(Span::styled(format!("{} · ", w.path.display()), dim()));
        l1.push(Span::raw(format!(
            "{} {}{} · upstream {} · HEAD {}",
            w.branch.as_deref().unwrap_or("(detached)"),
            w.sync_glyph(),
            if w.dirty { " dirty" } else { " clean" },
            w.upstream.as_deref().unwrap_or("—"),
            w.head
        )));
    } else {
        l1.push(Span::styled(
            format!("{} (not a worktree of a configured repo)", r.dir.display()),
            dim(),
        ));
    }
    lines.push(Line::from(l1));
    for a in &r.agents {
        let pane = r
            .panes
            .iter()
            .find(|p| p.command == "opencode")
            .map(|p| p.target())
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{} {}", a.activity.glyph(), a.activity.label()),
                activity_style(a.activity),
            ),
            Span::raw(format!(
                " {} · {}",
                a.mode.as_deref().unwrap_or("?"),
                trunc(a.title.as_deref().unwrap_or("(no title)"), 70)
            )),
            Span::styled(
                format!(
                    " · {} · {} ago{}",
                    pane,
                    util::ago(a.last_activity),
                    a.port.map(|p| format!(" · port {p}")).unwrap_or_default()
                ),
                dim(),
            ),
        ]));
        if let Some(u) = &a.plan_url {
            lines.push(Line::from(vec![
                Span::raw("     plan: "),
                Span::styled(u.clone(), Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("  {}", a.plan_title.as_deref().unwrap_or("")),
                    dim(),
                ),
            ]));
        }
    }
    for x in &r.reviews {
        lines.push(Line::from(vec![
            Span::raw(format!("   {} ", x.display_id)),
            Span::styled(x.status.label().to_string(), review_style(x.status)),
            Span::raw(format!(
                " · CI {} · approved by {} · {} comments, {} resolved, {} open · ",
                x.ci.glyph(),
                if x.approvals.is_empty() {
                    "nobody".into()
                } else {
                    x.approvals.join(", ")
                },
                x.threads.total,
                x.threads.resolved,
                x.threads.unresolved
            )),
            Span::styled(x.url.clone(), dim()),
        ]));
    }
    Text::from(lines)
}
