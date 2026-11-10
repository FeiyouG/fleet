//! Reviews tab: [project section] → review → local worktrees → agents.

use super::{
    in_machine_scope, machine_cell, render_table, state_cell, tree_label, where_cell, Node,
    NodeData, RowKey, TreeNode,
};
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
        // A machine scope keeps reviews with a worktree on that machine.
        .filter(|r| {
            matches!(app.tabs.machine, super::MachineScope::All)
                || r.worktrees
                    .iter()
                    .any(|w| super::in_machine_scope(app, w.machine.as_deref()))
        })
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
    // Reviews are ordered by MR/PR number, newest (highest) first, in every mode: that is how
    // people refer to them and how the forge lists them. Severity only groups by status first.
    match app.sort {
        SortMode::Severity => rows.sort_by(|a, b| {
            let score = |r: &ReviewView| match r.review.status {
                ReviewStatus::CiFailed | ReviewStatus::Conflicts => 0,
                ReviewStatus::Open => 1,
                ReviewStatus::Approved | ReviewStatus::Mergeable => 2,
                ReviewStatus::Draft => 3,
                _ => 4,
            };
            score(a).cmp(&score(b)).then(b.review.id.cmp(&a.review.id))
        }),
        SortMode::Activity => rows.sort_by(|a, b| b.review.updated_at.cmp(&a.review.updated_at)),
        SortMode::Name => rows.sort_by(|a, b| b.review.id.cmp(&a.review.id)),
    }
    rows
}

/// Review → Agent (spec tui-machines). Each agent row carries its worktree, so it shows
/// where the agent runs; a local worktree with no agent stays as a leaf (it can be closed).
fn review_node(app: &App, v: &ReviewView) -> TreeNode {
    let wt_of = |a: &fleet_core::model::AgentRow| {
        v.worktrees
            .iter()
            .find(|w| w.machine == a.machine && a.cwd.starts_with(&w.path))
    };
    let mut children: Vec<TreeNode> = v
        .agents
        .iter()
        .filter(|a| in_machine_scope(app, a.machine.as_deref()))
        .map(|a| {
            let w = wt_of(a);
            let pane = w
                .and_then(|w| {
                    app.fleet
                        .worktrees
                        .iter()
                        .find(|x| x.worktree.path == w.path)
                })
                .and_then(|vw| {
                    a.pane_target
                        .as_ref()
                        .and_then(|t| vw.panes.iter().find(|p| p.target() == *t))
                        .cloned()
                });
            TreeNode::leaf(
                RowKey::Agent(a.pid),
                NodeData::Agent {
                    agent: a.clone(),
                    worktree: w.cloned(),
                    review: Some(v.review.clone()),
                    pane,
                    project: w.map(|w| w.repo.clone()),
                },
            )
        })
        .collect();
    for w in v.worktrees.iter().filter(|w| {
        in_machine_scope(app, w.machine.as_deref())
            && !v
                .agents
                .iter()
                .any(|a| wt_of(a).is_some_and(|x| x.path == w.path))
    }) {
        let view = app
            .fleet
            .worktrees
            .iter()
            .find(|x| x.worktree.path == w.path)
            .cloned()
            .unwrap_or_else(|| fleet_core::model::WorktreeView {
                worktree: w.clone(),
                overall: fleet_core::model::Overall::Dead,
                agents: vec![],
                panes: vec![],
                review: Some(v.review.clone()),
                gc: fleet_core::model::GcVerdict::No("unknown".into()),
            });
        children.push(TreeNode::leaf(
            RowKey::Dir(w.path.clone()),
            NodeData::Worktree(view),
        ));
    }
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
        None => {
            app.cfg
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
                    // A source whose fetch failed must not read as "0 open".
                    let failed =
                        app.fleet.warnings.iter().find_map(|w| {
                            w.strip_prefix("reviews: ").filter(|w| w.starts_with(src))
                        });
                    let summary = match failed {
                        Some(e) if mine.is_empty() => {
                            format!(
                                "{src} · ⚠ {} · {local} local",
                                e.trim_start_matches(src).trim_start_matches(": ")
                            )
                        }
                        _ => format!("{src} · {} open · {local} local", mine.len()),
                    };
                    Some(TreeNode::new(
                        RowKey::Project(p.name.clone()),
                        NodeData::Project {
                            name: p.name.clone(),
                            summary,
                        },
                        mine.iter().map(|v| review_node(app, v)).collect(),
                    ))
                })
                .collect()
        }
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, nodes: &[Node], selected: usize) {
    let rows: Vec<Row> = nodes
        .iter()
        .map(|n| match &n.data {
            NodeData::Project { name, summary } => {
                // summary is "source · N open · M local"
                let mut parts = summary.splitn(3, " · ");
                let source = parts.next().unwrap_or("").to_string();
                let open = parts.next().unwrap_or("").to_string();
                let local = parts.next().unwrap_or("").to_string();
                // the middle part is either "N open" or "⚠ <error>"
                let (open_text, open_style) = if open.starts_with('⚠') {
                    (open.clone(), Style::default().fg(Color::Red))
                } else {
                    (open.clone(), dim())
                };
                Row::new(vec![
                    Cell::from(Span::styled(
                        if n.unfolded {
                            "▾ project"
                        } else {
                            "▸ project"
                        },
                        dim(),
                    )),
                    Cell::from(Span::styled(
                        name.clone(),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(Color::Cyan),
                    )),
                    Cell::from(""),
                    Cell::from(Span::styled(open_text, open_style)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Span::styled(format!("{local} · {source}"), dim())),
                ])
            }
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
                let author = Span::styled(
                    trunc(&r.author, 14),
                    if is_me(&r.author) {
                        Style::default().fg(Color::Cyan)
                    } else {
                        dim()
                    },
                );
                Row::new(vec![
                    Cell::from(tree_label(
                        n,
                        &format!("{} {:<6}", util::icon::REVIEW, r.display_id),
                    )),
                    Cell::from(trunc(&r.title, 44)),
                    Cell::from(author),
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
                    Cell::from(tree_label(n, &format!("{} wt", util::icon::BRANCH))),
                    Cell::from(Line::from(vec![
                        Span::raw(trunc(&w.name, 24)),
                        Span::styled(
                            format!(
                                " · {} {}{} · no agent",
                                w.branch.as_deref().unwrap_or("(detached)"),
                                w.sync_glyph(),
                                if w.dirty { " *" } else { "" }
                            ),
                            dim(),
                        ),
                    ])),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    machine_cell(app, w.machine.as_deref()),
                ])
            }
            NodeData::Agent {
                agent,
                worktree,
                pane,
                ..
            } => Row::new(vec![
                Cell::from(tree_label(n, agent.mode.as_deref().unwrap_or("agent"))),
                Cell::from(Line::from(vec![
                    Span::raw(trunc(agent.title.as_deref().unwrap_or("(no title)"), 26)),
                    Span::styled(
                        worktree
                            .as_ref()
                            .map(|w| format!(" · {}", w.name))
                            .unwrap_or_default(),
                        dim(),
                    ),
                ])),
                Cell::from(Span::styled(
                    format!("{} ago", util::ago(agent.last_activity)),
                    dim(),
                )),
                state_cell(app, agent),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
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
            "ID",
            "TITLE",
            "AUTHOR",
            "STATUS",
            "CI",
            "👍",
            "COMMENTS",
            "LOCAL / WHERE",
        ],
        vec![
            Constraint::Length(14),
            Constraint::Length(44),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(12),
            Constraint::Min(34),
        ],
        rows,
        selected,
    );
}
