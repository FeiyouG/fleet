//! Tmux tab: session (folded by default) → panes.

use super::{agent_cell, render_table, tree_label, Node, NodeData, RowKey, TreeNode};
use crate::tui::app::App;
use crate::tui::util::{self, dim, trunc};
use fleet_core::model::{Activity, PaneView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};
use std::collections::BTreeMap;

/// Sessions in scope: all, or those with at least one pane in the project's worktrees.
pub fn build(app: &App) -> Vec<TreeNode> {
    let mut by_session: BTreeMap<String, Vec<&PaneView>> = BTreeMap::new();
    for p in &app.fleet.panes {
        by_session
            .entry(p.pane.session.clone())
            .or_default()
            .push(p);
    }
    let mut out = vec![];
    for (session, panes) in by_session {
        if let Some(proj) = &app.tabs.project {
            if !panes.iter().any(|p| {
                p.worktree
                    .as_ref()
                    .map(|w| w.repo == *proj)
                    .unwrap_or(false)
            }) {
                continue;
            }
        }
        let mut sorted: Vec<PaneView> = panes.iter().map(|p| (*p).clone()).collect();
        sorted.sort_by_key(|p| (p.pane.window, p.pane.pane));
        let attached = sorted.iter().any(|p| p.pane.attached);
        let worktree = sorted.iter().find_map(|p| p.worktree.clone());
        let agents = sorted.iter().filter(|p| p.agent.is_some()).count();
        let urgent = sorted
            .iter()
            .filter_map(|p| p.agent.as_ref().map(|a| a.activity))
            .min();
        let children = sorted
            .iter()
            .map(|p| TreeNode::leaf(RowKey::Pane(p.pane.target()), NodeData::Pane(p.clone())))
            .collect();
        out.push(TreeNode::new(
            RowKey::Session(session.clone()),
            NodeData::Session {
                name: session,
                attached,
                panes: sorted,
                worktree,
                agents,
                urgent,
            },
            children,
        ));
    }
    // most urgent first
    out.sort_by_key(|n| match &n.data {
        NodeData::Session { urgent, name, .. } => {
            (urgent.map(|a| a as u8).unwrap_or(99), name.clone())
        }
        _ => (99, String::new()),
    });
    out
}

pub fn draw(f: &mut Frame, area: Rect, _app: &mut App, nodes: &[Node], selected: usize) {
    let home = fleet_core::config::home().to_string_lossy().into_owned();
    let rows: Vec<Row> = nodes
        .iter()
        .map(|n| match &n.data {
            NodeData::Session {
                name,
                attached,
                panes,
                worktree,
                agents,
                urgent,
            } => {
                let summary = Line::from(vec![
                    Span::styled(format!("{} panes · {} agents", panes.len(), agents), dim()),
                    match urgent {
                        Some(a) => Span::styled(
                            format!(" · {} {}", a.glyph(), a.label()),
                            util::activity_style(*a),
                        ),
                        None => Span::raw(""),
                    },
                ]);
                Row::new(vec![
                    Cell::from(tree_label(n, name)),
                    Cell::from(Span::styled(
                        if *attached { "●" } else { "○" },
                        if *attached {
                            Style::default().fg(Color::Green)
                        } else {
                            dim()
                        },
                    )),
                    Cell::from(""),
                    Cell::from(
                        worktree
                            .as_ref()
                            .map(|w| w.name.clone())
                            .unwrap_or_default(),
                    ),
                    Cell::from(summary),
                    Cell::from(Span::styled(
                        trunc(
                            &panes
                                .first()
                                .map(|p| p.pane.cwd.to_string_lossy().replace(&home, "~"))
                                .unwrap_or_default(),
                            44,
                        ),
                        dim(),
                    )),
                ])
            }
            NodeData::Pane(p) => {
                let title = if p.agent.is_some() {
                    p.agent
                        .as_ref()
                        .and_then(|a| a.title.clone())
                        .unwrap_or_else(|| p.pane.title.clone())
                } else if p.pane.title.is_empty() || !p.pane.title.contains(' ') {
                    p.pane.cwd.to_string_lossy().replace(&home, "~")
                } else {
                    p.pane.title.clone()
                };
                Row::new(vec![
                    Cell::from(tree_label(
                        n,
                        &format!("{}:{} {}", p.pane.window, p.pane.pane, p.pane.window_name),
                    )),
                    Cell::from(""),
                    Cell::from(p.pane.command.clone()),
                    Cell::from(
                        p.worktree
                            .as_ref()
                            .map(|w| w.name.clone())
                            .unwrap_or_default(),
                    ),
                    match &p.agent {
                        Some(a) => agent_cell(a),
                        None => Cell::from(Span::styled("—", dim())),
                    },
                    Cell::from(trunc(&title, 48)),
                ])
            }
            _ => Row::new(vec![Cell::from("")]),
        })
        .collect();
    let _ = Activity::Idle;
    render_table(
        f,
        area,
        vec![
            "SESSION / WINDOW:PANE",
            "ATT",
            "COMMAND",
            "WORKTREE",
            "AGENT",
            "TITLE / CWD",
        ],
        vec![
            Constraint::Length(34),
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Length(26),
            Constraint::Length(36),
            Constraint::Min(20),
        ],
        rows,
        selected,
    );
}
