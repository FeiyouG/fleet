//! Worktrees tab: [project section] → worktree, plus "outside projects" (agents in no
//! project's worktree).

use super::{
    in_machine_scope, machine_cell, overall_cell, render_table, review_cell, state_cell,
    state_spans, tree_label, where_cell, Node, NodeData, RowKey, TreeNode, OUTSIDE,
};
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, dim, trunc};
use fleet_core::model::{Overall, WorktreeView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

/// Worktrees of the scoped project(s), sorted.
pub fn scoped(app: &App) -> Vec<&WorktreeView> {
    let mut v: Vec<&WorktreeView> = app
        .fleet
        .worktrees
        .iter()
        .filter(|w| in_machine_scope(app, w.worktree.machine.as_deref()))
        .filter(|w| {
            app.tabs
                .project
                .as_deref()
                .map(|p| w.worktree.repo == p)
                .unwrap_or(true)
        })
        .collect();
    match app.sort {
        SortMode::Severity => v.sort_by(|a, b| {
            a.overall
                .cmp(&b.overall)
                .then(
                    b.agents
                        .iter()
                        .filter_map(|x| x.last_activity)
                        .max()
                        .cmp(&a.agents.iter().filter_map(|x| x.last_activity).max()),
                )
                .then(a.worktree.name.cmp(&b.worktree.name))
        }),
        SortMode::Activity => v.sort_by(|a, b| {
            b.agents
                .iter()
                .filter_map(|x| x.last_activity)
                .max()
                .cmp(&a.agents.iter().filter_map(|x| x.last_activity).max())
        }),
        SortMode::Name => v.sort_by(|a, b| a.worktree.name.cmp(&b.worktree.name)),
    }
    v
}

fn worktree_node(v: &WorktreeView) -> TreeNode {
    TreeNode::leaf(
        RowKey::Dir(v.worktree.path.clone()),
        NodeData::Worktree(v.clone()),
    )
}

fn section_summary(wts: &[&WorktreeView]) -> String {
    let needs = wts
        .iter()
        .filter(|w| w.overall == Overall::NeedsYou)
        .count();
    let agents: usize = wts.iter().map(|w| w.agents.len()).sum();
    format!(
        "{} worktrees · {} agents · {} need you",
        wts.len(),
        agents,
        needs
    )
}

pub fn build(app: &App) -> Vec<TreeNode> {
    let wts = scoped(app);
    let mut out = vec![];
    match &app.tabs.project {
        Some(_) => out.extend(wts.iter().map(|v| worktree_node(v))),
        None => {
            for p in &app.cfg.projects {
                let mine: Vec<&WorktreeView> = wts
                    .iter()
                    .copied()
                    .filter(|w| w.worktree.repo == p.name)
                    .collect();
                out.push(TreeNode::new(
                    RowKey::Project(p.name.clone()),
                    NodeData::Project {
                        name: p.name.clone(),
                        summary: section_summary(&mine),
                    },
                    mine.iter().map(|v| worktree_node(v)).collect(),
                ));
            }
            // Agents outside every project: a section whose children are the agents.
            let outside: Vec<TreeNode> = app
                .fleet
                .overview
                .iter()
                .filter(|r| r.worktree.is_none())
                .flat_map(|r| {
                    r.agents
                        .iter()
                        .filter(|a| in_machine_scope(app, a.machine.as_deref()))
                        .map(move |a| {
                            let pane = a
                                .pane_target
                                .as_ref()
                                .and_then(|t| r.panes.iter().find(|p| p.target() == *t))
                                .cloned();
                            TreeNode::leaf(
                                RowKey::Agent(a.pid),
                                NodeData::Agent {
                                    agent: a.clone(),
                                    worktree: None,
                                    review: None,
                                    pane,
                                    project: None,
                                },
                            )
                        })
                })
                .collect();
            if !outside.is_empty() {
                let n = outside.len();
                out.push(TreeNode::new(
                    RowKey::Project(OUTSIDE.into()),
                    NodeData::Project {
                        name: OUTSIDE.into(),
                        summary: format!("{n} agents in directories of no configured project"),
                    },
                    outside,
                ));
            }
        }
    }
    out
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, nodes: &[Node], selected: usize) {
    let show_paths = app.tabs.show_paths;
    let home = fleet_core::config::home().to_string_lossy().into_owned();
    let name_w = if show_paths { 48 } else { 28 };
    let rows: Vec<Row> = nodes
        .iter()
        .map(|n| match &n.data {
            NodeData::Project { name, summary } => Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(if n.unfolded { "▾ " } else { "▸ " }, dim()),
                    Span::styled(
                        name.clone(),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(Color::Cyan),
                    ),
                ])),
                Cell::from(Span::styled(summary.clone(), dim())),
            ]),
            NodeData::Worktree(v) => {
                let w = &v.worktree;
                let name = if show_paths {
                    w.path.to_string_lossy().replace(&home, "~")
                } else {
                    w.name.clone()
                };
                let agents = match v.agents.first() {
                    Some(a) => {
                        let mut c = vec![Span::styled(
                            format!("{:<5} ", a.mode.as_deref().unwrap_or("?")),
                            dim(),
                        )];
                        c.extend(state_spans(app, a));
                        if v.agents.len() > 1 {
                            c.push(Span::styled(format!(" +{}", v.agents.len() - 1), dim()));
                        }
                        Cell::from(Line::from(c))
                    }
                    None => Cell::from(Span::styled("—", dim())),
                };
                Row::new(vec![
                    Cell::from(tree_label(n, &trunc(&name, name_w))),
                    Cell::from(Span::raw(trunc(
                        w.branch.as_deref().unwrap_or("(detached)"),
                        30,
                    ))),
                    Cell::from(Span::styled(w.sync_glyph(), dim())),
                    Cell::from(if w.dirty {
                        Span::styled("modified", Style::default().fg(Color::Yellow))
                    } else {
                        Span::styled("clean", dim())
                    }),
                    overall_cell(v.overall, &v.gc),
                    review_cell(v.review.as_ref()),
                    agents,
                    machine_cell(app, w.machine.as_deref()),
                ])
            }
            NodeData::Agent { agent, pane, .. } => Row::new(vec![
                Cell::from(tree_label(
                    n,
                    &trunc(agent.title.as_deref().unwrap_or("(no title)"), name_w),
                )),
                Cell::from(""),
                Cell::from(""),
                Cell::from(Span::styled(
                    format!("{} ago", util::ago(agent.last_activity)),
                    dim(),
                )),
                state_cell(app, agent),
                Cell::from(""),
                Cell::from(Span::styled(
                    agent.mode.as_deref().unwrap_or("?").to_string(),
                    dim(),
                )),
                where_cell(
                    app,
                    agent.machine.as_deref(),
                    agent.pane_target.as_deref(),
                    pane.as_ref().map(|p| p.attached).unwrap_or(false),
                ),
            ]),
            _ => Row::new(vec![Cell::from("")]),
        })
        .collect();
    render_table(
        f,
        area,
        vec![
            if show_paths { "PATH" } else { "WORKTREE" },
            "BRANCH",
            "SYNC",
            "CHANGES",
            "STATE",
            "REVIEW",
            "AGENTS",
            "MACHINE",
        ],
        vec![
            Constraint::Length(name_w as u16 + 4),
            Constraint::Length(30),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(16),
            Constraint::Min(22),
            Constraint::Length(20),
            Constraint::Length(30),
        ],
        rows,
        selected,
    );
}
