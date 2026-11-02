//! Reviews tab: [project section] → review → local worktrees → agents.

use super::{agent_cell, pane_cell, render_table, tree_label, Node, NodeData, RowKey, TreeNode};
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, dim, review_style, trunc};
use fleet_core::model::{ReviewStatus, ReviewView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

pub const PRESETS: &[&str] = &["all", "local", "mine", "others", "needs-me", "failed"];

/// The user's forge identities (`glab` and `gh`), cached once per process.
fn me() -> &'static [String] {
    static ME: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        let mut v = vec![];
        if let Some(u) = std::process::Command::new("glab")
            .args(["api", "user"])
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
            .and_then(|j| j.get("username").and_then(|u| u.as_str()).map(String::from))
        {
            v.push(u);
        }
        if let Some(u) = std::process::Command::new("gh")
            .args(["api", "user", "--jq", ".login"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
        {
            v.push(u);
        }
        v
    })
}

pub fn is_me(user: &str) -> bool {
    me().iter().any(|m| m == user)
}

/// Reviews of the scoped project(s) after the preset filter, sorted.
pub fn scoped(app: &App) -> Vec<&ReviewView> {
    let preset = PRESETS[app.tabs.reviews.preset];
    let sources: Vec<&str> = match &app.tabs.project {
        Some(p) => app
            .cfg
            .project(p)
            .and_then(|x| x.reviews.as_deref())
            .into_iter()
            .collect(),
        None => app
            .cfg
            .projects
            .iter()
            .filter_map(|p| p.reviews.as_deref())
            .collect(),
    };
    let mut rows: Vec<&ReviewView> = app
        .fleet
        .reviews
        .iter()
        .filter(|r| sources.contains(&r.review.source.as_str()))
        .filter(|r| match preset {
            "local" => !r.worktrees.is_empty(),
            "mine" => is_me(&r.review.author),
            "others" => !is_me(&r.review.author),
            "needs-me" => {
                !is_me(&r.review.author)
                    && !r.review.approvals.iter().any(|a| is_me(a))
                    && !r.review.status.is_done()
            }
            "failed" => matches!(
                r.review.status,
                ReviewStatus::CiFailed | ReviewStatus::Conflicts
            ),
            _ => true,
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

fn review_node(app: &App, v: &ReviewView) -> TreeNode {
    let children: Vec<TreeNode> = v
        .worktrees
        .iter()
        .map(|w| {
            let view = app
                .fleet
                .worktrees
                .iter()
                .find(|x| x.worktree.path == w.path);
            let agents: Vec<TreeNode> = v
                .agents
                .iter()
                .filter(|a| a.cwd.starts_with(&w.path))
                .map(|a| {
                    let pane = view.and_then(|vw| {
                        a.pane_target
                            .as_ref()
                            .and_then(|t| vw.panes.iter().find(|p| p.target() == *t))
                            .cloned()
                    });
                    TreeNode::leaf(
                        RowKey::Agent(a.pid),
                        NodeData::Agent {
                            agent: a.clone(),
                            worktree: Some(w.clone()),
                            review: Some(v.review.clone()),
                            pane,
                            project: Some(w.repo.clone()),
                        },
                    )
                })
                .collect();
            match view {
                Some(vw) => TreeNode::new(
                    RowKey::Dir(w.path.clone()),
                    NodeData::Worktree(vw.clone()),
                    agents,
                ),
                None => TreeNode::new(
                    RowKey::Dir(w.path.clone()),
                    NodeData::Worktree(fleet_core::model::WorktreeView {
                        worktree: w.clone(),
                        overall: fleet_core::model::Overall::Dead,
                        agents: vec![],
                        panes: vec![],
                        review: Some(v.review.clone()),
                        gc: fleet_core::model::GcVerdict::No("unknown".into()),
                    }),
                    agents,
                ),
            }
        })
        .collect();
    TreeNode::new(
        RowKey::Review(v.review.source.clone(), v.review.id),
        NodeData::Review(v.clone()),
        children,
    )
}

pub fn build(app: &App) -> Vec<TreeNode> {
    let rows = scoped(app);
    match &app.tabs.project {
        Some(_) => rows.iter().map(|v| review_node(app, v)).collect(),
        None => app
            .cfg
            .projects
            .iter()
            .filter_map(|p| {
                let src = p.reviews.as_deref()?;
                let mine: Vec<&ReviewView> = rows
                    .iter()
                    .copied()
                    .filter(|r| r.review.source == src)
                    .collect();
                let local = mine.iter().filter(|r| !r.worktrees.is_empty()).count();
                Some(TreeNode::new(
                    RowKey::Project(p.name.clone()),
                    NodeData::Project {
                        name: p.name.clone(),
                        summary: format!("{src} · {} open · {local} local", mine.len()),
                    },
                    mine.iter().map(|v| review_node(app, v)).collect(),
                ))
            })
            .collect(),
    }
}

pub fn draw(f: &mut Frame, area: Rect, _app: &mut App, nodes: &[Node], selected: usize) {
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
            NodeData::Review(v) => {
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
                let local = match (v.worktrees.len(), v.agents.first()) {
                    (0, _) => Line::from(Span::styled("—", dim())),
                    (w, None) => {
                        Line::from(format!("{w} worktree{}", if w > 1 { "s" } else { "" }))
                    }
                    (w, Some(a)) => Line::from(vec![
                        Span::raw(format!("{w} wt · {} ag · ", v.agents.len())),
                        Span::styled(
                            format!("{} {}", a.activity.glyph(), a.activity.label()),
                            util::activity_style(a.activity),
                        ),
                    ]),
                };
                let author = if is_me(&r.author) {
                    "me".to_string()
                } else {
                    trunc(&r.author, 10)
                };
                Row::new(vec![
                    Cell::from(tree_label(n, &r.display_id)),
                    Cell::from(trunc(&r.title, 44)),
                    Cell::from(Span::styled(author, dim())),
                    Cell::from(Span::styled(
                        r.status.label().to_string(),
                        review_style(r.status),
                    )),
                    Cell::from(r.ci.glyph()),
                    Cell::from(format!("{}", r.approvals.len())),
                    Cell::from(comments),
                    Cell::from(local),
                ])
            }
            NodeData::Worktree(v) => {
                let w = &v.worktree;
                Row::new(vec![
                    Cell::from(tree_label(n, &trunc(&w.name, 30))),
                    Cell::from(Line::from(vec![
                        Span::raw(w.branch.as_deref().unwrap_or("(detached)").to_string()),
                        Span::styled(
                            format!(" {}{}", w.sync_glyph(), if w.dirty { " *" } else { "" }),
                            dim(),
                        ),
                    ])),
                    Cell::from(""),
                    Cell::from(Span::styled(
                        v.overall.label().to_string(),
                        util::overall_style(v.overall),
                    )),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Span::styled(format!("{} agents", v.agents.len()), dim())),
                    pane_cell(
                        v.agents.first().and_then(|a| a.pane_target.as_deref()).or(v
                            .panes
                            .first()
                            .map(|p| p.target())
                            .as_deref()),
                        v.panes.iter().any(|p| p.attached),
                    ),
                ])
            }
            NodeData::Agent { agent, pane, .. } => Row::new(vec![
                Cell::from(tree_label(
                    n,
                    &trunc(agent.title.as_deref().unwrap_or("(no title)"), 30),
                )),
                agent_cell(agent),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(Span::styled(
                    format!("{} ago", util::ago(agent.last_activity)),
                    dim(),
                )),
                pane_cell(
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
            "ID", "TITLE", "AUTHOR", "STATUS", "CI", "👍", "COMMENTS", "LOCAL",
        ],
        vec![
            Constraint::Length(32),
            Constraint::Length(44),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(12),
            Constraint::Min(26),
        ],
        rows,
        selected,
    );
}
