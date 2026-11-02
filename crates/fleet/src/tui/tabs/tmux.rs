use super::render_table;
use crate::tui::app::App;
use crate::tui::util::{self, activity_style, dim, trunc};
use fleet_core::model::{AgentRow, WorktreeRow};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};
use std::collections::BTreeMap;

/// One visible line in the tree: a session header, or a pane.
#[derive(Debug, Clone)]
pub struct TreeRow {
    pub session: String,
    pub target: String,
    pub is_session: bool,
    pub attached: bool,
    pub window: Option<(u32, String)>,
    pub pane: Option<u32>,
    pub command: String,
    pub title: String,
    pub cwd: std::path::PathBuf,
    pub worktree: Option<WorktreeRow>,
    pub agent: Option<AgentRow>,
    pub agent_count: usize,
}

pub fn visible(app: &App) -> Vec<TreeRow> {
    let st = &app.tabs.tmux;
    let mut by_session: BTreeMap<String, Vec<&fleet_core::model::PaneView>> = BTreeMap::new();
    for p in &app.fleet.panes {
        by_session
            .entry(p.pane.session.clone())
            .or_default()
            .push(p);
    }
    let mut out = vec![];
    for (session, panes) in by_session {
        let hay = format!(
            "{session} {}",
            panes
                .iter()
                .map(|p| format!(
                    "{} {} {}",
                    p.pane.command,
                    p.pane.title,
                    p.agent
                        .as_ref()
                        .and_then(|a| a.title.clone())
                        .unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if !util::fuzzy_match(&hay, &st.filter) {
            continue;
        }
        let attached = panes.iter().any(|p| p.pane.attached);
        let wt = panes.iter().find_map(|p| p.worktree.clone());
        let agent_count = panes.iter().filter(|p| p.agent.is_some()).count();
        out.push(TreeRow {
            session: session.clone(),
            target: session.clone(),
            is_session: true,
            attached,
            window: None,
            pane: None,
            command: String::new(),
            title: String::new(),
            cwd: panes
                .first()
                .map(|p| p.pane.cwd.clone())
                .unwrap_or_default(),
            worktree: wt,
            agent: panes.iter().find_map(|p| p.agent.clone()),
            agent_count,
        });
        // Expanded by default; `h` collapses every session to one row, `l` expands.
        if st.expanded {
            let mut sorted = panes.clone();
            sorted.sort_by_key(|p| (p.pane.window, p.pane.pane));
            for p in sorted {
                out.push(TreeRow {
                    session: session.clone(),
                    target: p.pane.target(),
                    is_session: false,
                    attached: p.pane.attached,
                    window: Some((p.pane.window, p.pane.window_name.clone())),
                    pane: Some(p.pane.pane),
                    command: p.pane.command.clone(),
                    title: p.pane.title.clone(),
                    cwd: p.pane.cwd.clone(),
                    worktree: p.worktree.clone(),
                    agent: p.agent.clone(),
                    agent_count: usize::from(p.agent.is_some()),
                });
            }
        }
    }
    out
}

pub fn selected(app: &App) -> Option<TreeRow> {
    visible(app).into_iter().nth(app.tabs.tmux.selected)
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let rows_v = visible(app);
    let home = fleet_core::config::home().to_string_lossy().into_owned();
    let rows: Vec<Row> = rows_v
        .iter()
        .map(|r| {
            if r.is_session {
                Row::new(vec![
                    Cell::from(Line::from(vec![
                        Span::styled(
                            if app.tabs.tmux.expanded {
                                "▾ "
                            } else {
                                "▸ "
                            },
                            dim(),
                        ),
                        Span::styled(
                            r.session.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ])),
                    Cell::from(Span::styled(
                        if r.attached { "●" } else { "○" },
                        if r.attached {
                            Style::default().fg(Color::Green)
                        } else {
                            dim()
                        },
                    )),
                    Cell::from(""),
                    Cell::from(
                        r.worktree
                            .as_ref()
                            .map(|w| w.name.clone())
                            .unwrap_or_default(),
                    ),
                    Cell::from(Span::styled(
                        match r.agent_count {
                            0 => "".to_string(),
                            1 => "1 agent".into(),
                            n => format!("{n} agents"),
                        },
                        dim(),
                    )),
                    Cell::from(Span::styled(
                        trunc(&r.cwd.to_string_lossy().replace(&home, "~"), 40),
                        dim(),
                    )),
                ])
            } else {
                let (wi, wn) = r.window.clone().unwrap_or((0, String::new()));
                let agent = r
                    .agent
                    .as_ref()
                    .map(|a| {
                        Line::from(vec![
                            Span::styled(
                                format!("{:<5} ", a.mode.as_deref().unwrap_or("?")),
                                dim(),
                            ),
                            Span::styled(
                                format!("{} {}", a.activity.glyph(), a.activity.label()),
                                activity_style(a.activity),
                            ),
                        ])
                    })
                    .unwrap_or_else(|| Line::from(Span::styled("—", dim())));
                let title = if r.command == "opencode" {
                    r.agent
                        .as_ref()
                        .and_then(|a| a.title.clone())
                        .unwrap_or_else(|| r.title.clone())
                } else if r.title.is_empty() || r.title.contains('.') && !r.title.contains(' ') {
                    r.cwd.to_string_lossy().replace(&home, "~")
                } else {
                    r.title.clone()
                };
                Row::new(vec![
                    Cell::from(Span::styled(
                        format!("    {wi}:{} {wn}", r.pane.unwrap_or(0)),
                        dim(),
                    )),
                    Cell::from(""),
                    Cell::from(r.command.clone()),
                    Cell::from(
                        r.worktree
                            .as_ref()
                            .map(|w| w.name.clone())
                            .unwrap_or_default(),
                    ),
                    Cell::from(agent),
                    Cell::from(trunc(&title, 48)),
                ])
            }
        })
        .collect();
    render_table(
        f,
        area,
        vec![
            "SESSION / WINDOW.PANE",
            "ATT",
            "COMMAND",
            "WORKTREE",
            "AGENT",
            "TITLE / CWD",
        ],
        vec![
            Constraint::Length(36),
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Length(28),
            Constraint::Length(22),
            Constraint::Min(20),
        ],
        rows,
        app.tabs.tmux.selected,
    );
}

pub fn detail(app: &App) -> Text<'static> {
    let Some(r) = selected(app) else {
        return Text::from("");
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} ", r.target), util::accent()),
        Span::raw(format!(
            "{} · cwd {}",
            if r.attached { "attached" } else { "detached" },
            r.cwd.display()
        )),
    ])];
    if let Some(w) = &r.worktree {
        lines.push(Line::from(format!(
            "   worktree {} · {} {}",
            w.name,
            w.branch.as_deref().unwrap_or("(detached)"),
            w.sync_glyph()
        )));
    }
    if let Some(a) = &r.agent {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{} {}", a.activity.glyph(), a.activity.label()),
                activity_style(a.activity),
            ),
            Span::raw(format!(
                " {} · {} · {} ago",
                a.mode.as_deref().unwrap_or("?"),
                a.title.as_deref().unwrap_or(""),
                util::ago(a.last_activity)
            )),
        ]));
        if let Some(u) = &a.plan_url {
            lines.push(Line::from(vec![
                Span::raw("     plan: "),
                Span::styled(u.clone(), Style::default().fg(Color::Yellow)),
            ]));
        }
    }
    Text::from(lines)
}
