//! Agents tab: flat, one row per running agent process.

use super::{
    agent_cell, branch_cell, pane_cell, render_table, review_cell, Node, NodeData, RowKey, TreeNode,
};
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, dim, trunc};
use fleet_core::model::OverviewRow;
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

fn in_scope(app: &App, r: &OverviewRow) -> bool {
    match &app.tabs.project {
        None => true,
        Some(p) => r.repo() == Some(p.as_str()),
    }
}

pub fn build(app: &App) -> Vec<TreeNode> {
    let mut rows: Vec<(TreeNode, (u8, i64, String))> = vec![];
    for r in app.fleet.overview.iter().filter(|r| in_scope(app, r)) {
        let review = r
            .reviews
            .iter()
            .find(|x| !x.status.is_done())
            .or(r.reviews.first())
            .cloned();
        for a in &r.agents {
            let pane = a
                .pane_target
                .as_ref()
                .and_then(|t| r.panes.iter().find(|p| p.target() == *t))
                .cloned();
            let sort = match app.sort {
                SortMode::Severity => (
                    a.activity as u8,
                    -a.last_activity.map(|t| t.timestamp()).unwrap_or(0),
                    String::new(),
                ),
                SortMode::Activity => (
                    0,
                    -a.last_activity.map(|t| t.timestamp()).unwrap_or(0),
                    String::new(),
                ),
                SortMode::Name => (0, 0, r.name()),
            };
            rows.push((
                TreeNode::leaf(
                    RowKey::Agent(a.pid),
                    NodeData::Agent {
                        agent: a.clone(),
                        worktree: r.worktree.clone(),
                        review: review.clone(),
                        pane,
                        project: r.repo().map(String::from),
                    },
                ),
                sort,
            ));
        }
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    rows.into_iter().map(|(n, _)| n).collect()
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, nodes: &[Node], selected: usize) {
    let show_project = app.tabs.project.is_none() && app.cfg.projects.len() > 1;
    let rows: Vec<Row> = nodes
        .iter()
        .filter_map(|n| match &n.data {
            NodeData::Agent {
                agent,
                worktree,
                review,
                pane,
                project,
            } => {
                let mut cells = vec![
                    agent_cell(agent),
                    Cell::from(trunc(agent.title.as_deref().unwrap_or("(no title)"), 40)),
                ];
                if show_project {
                    cells.push(Cell::from(Span::styled(
                        trunc(project.as_deref().unwrap_or("—"), 14),
                        dim(),
                    )));
                }
                cells.push(match worktree {
                    Some(w) => Cell::from(Line::from(vec![
                        Span::raw(trunc(&w.name, 24)),
                        Span::styled(
                            format!(" {}{}", w.sync_glyph(), if w.dirty { " *" } else { "" }),
                            dim(),
                        ),
                    ])),
                    None => Cell::from(Span::styled(
                        trunc(&agent.cwd.display().to_string(), 28),
                        dim(),
                    )),
                });
                let _ = branch_cell;
                cells.push(pane_cell(
                    agent.pane_target.as_deref(),
                    pane.as_ref().map(|p| p.attached).unwrap_or(false),
                ));
                cells.push(review_cell(review.as_ref()));
                cells.push(Cell::from(Span::styled(
                    util::ago(agent.last_activity),
                    dim(),
                )));
                Some(Row::new(cells))
            }
            _ => None,
        })
        .collect();
    let mut header = vec!["AGENT", "TITLE"];
    let mut widths = vec![Constraint::Length(21), Constraint::Length(40)];
    if show_project {
        header.push("PROJECT");
        widths.push(Constraint::Length(14));
    }
    header.extend(["WORKTREE", "TMUX", "REVIEW", "AGE"]);
    widths.extend([
        Constraint::Length(30),
        Constraint::Length(27),
        Constraint::Min(24),
        Constraint::Length(5),
    ]);
    render_table(f, area, header, widths, rows, selected);
}
