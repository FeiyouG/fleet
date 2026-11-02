//! Worktrees tab: [project section] → worktree → agents (when 2+), plus "outside projects".

use super::{
    agent_cell, branch_cell, pane_cell, render_table, review_cell, tree_label, Node, NodeData,
    RowKey, TreeNode, OUTSIDE,
};
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, dim, overall_style, trunc};
use fleet_core::model::{GcVerdict, Overall, WorktreeView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

/// Worktrees of the scoped project(s), sorted.
pub fn scoped(app: &App) -> Vec<&WorktreeView> {
    let mut v: Vec<&WorktreeView> = app
        .fleet
        .worktrees
        .iter()
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
    let children = if v.agents.len() >= 2 {
        v.agents
            .iter()
            .map(|a| {
                let pane = a
                    .pane_target
                    .as_ref()
                    .and_then(|t| v.panes.iter().find(|p| p.target() == *t))
                    .cloned();
                TreeNode::leaf(
                    RowKey::Agent(a.pid),
                    NodeData::Agent {
                        agent: a.clone(),
                        worktree: Some(v.worktree.clone()),
                        review: v.review.clone(),
                        pane,
                        project: Some(v.worktree.repo.clone()),
                    },
                )
            })
            .collect()
    } else {
        vec![]
    };
    TreeNode::new(
        RowKey::Dir(v.worktree.path.clone()),
        NodeData::Worktree(v.clone()),
        children,
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
                    r.agents.iter().map(move |a| {
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
                Cell::from(""),
                Cell::from(Line::from(vec![
                    Span::styled("▾ ".replace('▾', if n.unfolded { "▾" } else { "▸" }), dim()),
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
                let agent = match v.agents.first() {
                    Some(a) if v.agents.len() > 1 => Cell::from(Line::from(vec![
                        Span::styled(format!("{:<5} ", a.mode.as_deref().unwrap_or("?")), dim()),
                        Span::styled(
                            format!("{} {}", a.activity.glyph(), a.activity.label()),
                            util::activity_style(a.activity),
                        ),
                        Span::styled(format!(" +{}", v.agents.len() - 1), dim()),
                    ])),
                    Some(a) => agent_cell(a),
                    None => Cell::from(Span::styled("—     NONE", dim())),
                };
                let pane_t = v
                    .agents
                    .first()
                    .and_then(|a| a.pane_target.clone())
                    .or_else(|| v.panes.first().map(|p| p.target()));
                let attached = v.panes.iter().any(|p| p.attached);
                let gc = match &v.gc {
                    GcVerdict::Yes(r) => {
                        Span::styled(format!("gc: {r}"), Style::default().fg(Color::Yellow))
                    }
                    GcVerdict::No(_) => Span::raw(""),
                };
                Row::new(vec![
                    Cell::from(Span::styled(
                        v.overall.label().to_string(),
                        overall_style(v.overall),
                    )),
                    Cell::from(tree_label(n, &trunc(&name, name_w))),
                    branch_cell(w, 26),
                    agent,
                    pane_cell(pane_t.as_deref(), attached),
                    review_cell(v.review.as_ref()),
                    Cell::from(gc),
                ])
            }
            NodeData::Agent { agent, pane, .. } => Row::new(vec![
                Cell::from(""),
                Cell::from(tree_label(
                    n,
                    &trunc(agent.title.as_deref().unwrap_or("(no title)"), name_w),
                )),
                Cell::from(""),
                agent_cell(agent),
                pane_cell(
                    agent.pane_target.as_deref(),
                    pane.as_ref().map(|p| p.attached).unwrap_or(false),
                ),
                Cell::from(Span::styled(
                    format!("{} ago", util::ago(agent.last_activity)),
                    dim(),
                )),
                Cell::from(""),
            ]),
            _ => Row::new(vec![Cell::from("")]),
        })
        .collect();
    render_table(
        f,
        area,
        vec![
            "OVERALL",
            if show_paths { "PATH" } else { "NAME" },
            "BRANCH",
            "AGENT",
            "TMUX",
            "REVIEW",
            "",
        ],
        vec![
            Constraint::Length(10),
            Constraint::Length(name_w as u16),
            Constraint::Length(34),
            Constraint::Length(22),
            Constraint::Length(27),
            Constraint::Min(24),
            Constraint::Length(16),
        ],
        rows,
        selected,
    );
}
