//! Agents tab: [project section] → agent (spec tui-machines: Agents = Project → Agent; the
//! Project level hides only when the project scope pins it).

use super::{
    in_machine_scope, render_table, review_cell, state_cell, task_of, tree_label, where_cell, Node,
    NodeData, RowKey, TreeNode, OUTSIDE,
};
use crate::tui::app::{App, SortMode};
use crate::tui::util::{self, dim, trunc};
use fleet_core::model::{Activity, OverviewRow};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};

fn in_project(app: &App, r: &OverviewRow) -> bool {
    match &app.tabs.project {
        None => true,
        Some(p) => r.repo() == Some(p.as_str()),
    }
}

/// (project, row, sort key)
type Sorted = (Option<String>, TreeNode, (u8, i64, String));

/// Every agent in scope, sorted, with the project it belongs to.
fn agents(app: &App) -> Vec<(Option<String>, TreeNode)> {
    let mut rows: Vec<Sorted> = vec![];
    for r in app.fleet.overview.iter().filter(|r| in_project(app, r)) {
        let review = r
            .reviews
            .iter()
            .find(|x| !x.status.is_done())
            .or(r.reviews.first())
            .cloned();
        for a in r
            .agents
            .iter()
            .filter(|a| in_machine_scope(app, a.machine.as_deref()))
        {
            let pane = a
                .pane_target
                .as_ref()
                .and_then(|t| r.panes.iter().find(|p| p.target() == *t))
                .cloned();
            let age = -a.last_activity.map(|t| t.timestamp()).unwrap_or(0);
            let sort = match app.sort {
                SortMode::Severity => (a.activity as u8, age, String::new()),
                SortMode::Activity => (0, age, String::new()),
                SortMode::Name => (0, 0, r.name()),
            };
            let project = r.repo().map(String::from);
            rows.push((
                project.clone(),
                TreeNode::leaf(
                    RowKey::Agent(a.pid),
                    NodeData::Agent {
                        agent: a.clone(),
                        worktree: r.worktree.clone(),
                        review: review.clone(),
                        pane,
                        project,
                    },
                ),
                sort,
            ));
        }
    }
    rows.sort_by(|a, b| a.2.cmp(&b.2));
    rows.into_iter().map(|(p, n, _)| (p, n)).collect()
}

fn summary(nodes: &[TreeNode]) -> String {
    let acts: Vec<Activity> = nodes
        .iter()
        .filter_map(|n| match &n.data {
            NodeData::Agent { agent, .. } => Some(agent.activity),
            _ => None,
        })
        .collect();
    let needs = acts.iter().filter(|a| a.is_waiting()).count();
    let working = acts.iter().filter(|a| **a == Activity::Working).count();
    format!(
        "{} agents · {needs} need you · {working} working",
        acts.len()
    )
}

pub fn build(app: &App) -> Vec<TreeNode> {
    let mut all = agents(app);
    if app.tabs.project.is_some() {
        return all.into_iter().map(|(_, n)| n).collect();
    }
    let mut out = vec![];
    for p in &app.cfg.projects {
        let (mine, rest): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|(proj, _)| proj.as_deref() == Some(p.name.as_str()));
        all = rest;
        let mine: Vec<TreeNode> = mine.into_iter().map(|(_, n)| n).collect();
        out.push(TreeNode::new(
            RowKey::Project(p.name.clone()),
            NodeData::Project {
                name: p.name.clone(),
                summary: summary(&mine),
            },
            mine,
        ));
    }
    // What is left: agents in no configured project.
    if !all.is_empty() {
        let rest: Vec<TreeNode> = all.into_iter().map(|(_, n)| n).collect();
        out.push(TreeNode::new(
            RowKey::Project(OUTSIDE.into()),
            NodeData::Project {
                name: OUTSIDE.into(),
                summary: summary(&rest),
            },
            rest,
        ));
    }
    out
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, nodes: &[Node], selected: usize) {
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
                Cell::from(""),
                Cell::from(Span::styled(summary.clone(), dim())),
            ]),
            NodeData::Agent {
                agent,
                worktree,
                review,
                pane,
                ..
            } => Row::new(vec![
                Cell::from(tree_label(
                    n,
                    &format!(
                        "{} {}",
                        trunc(&agent.adapter, 9),
                        agent.mode.as_deref().unwrap_or("?")
                    ),
                )),
                state_cell(app, agent),
                Cell::from(trunc(agent.title.as_deref().unwrap_or("(no title)"), 38)),
                Cell::from(Span::styled(
                    trunc(task_of(app, agent).as_deref().unwrap_or("—"), 20),
                    dim(),
                )),
                where_cell(
                    app,
                    agent.machine.as_deref(),
                    agent.pane_target.as_deref(),
                    pane.as_ref().map(|p| p.attached).unwrap_or(false),
                ),
                match worktree {
                    Some(w) => Cell::from(Line::from(vec![
                        Span::raw(trunc(&w.name, 22)),
                        Span::styled(
                            format!(" {}{}", w.sync_glyph(), if w.dirty { " *" } else { "" }),
                            dim(),
                        ),
                    ])),
                    None => Cell::from(Span::styled(
                        trunc(&agent.cwd.display().to_string(), 26),
                        dim(),
                    )),
                },
                review_cell(review.as_ref()),
                Cell::from(Span::styled(util::ago(agent.last_activity), dim())),
            ]),
            _ => Row::new(vec![Cell::from("")]),
        })
        .collect();
    render_table(
        f,
        area,
        vec![
            "AGENT", "STATE", "TITLE", "TASK", "WHERE", "WORKTREE", "REVIEW", "AGE",
        ],
        vec![
            Constraint::Length(18),
            Constraint::Length(13),
            Constraint::Length(38),
            Constraint::Length(20),
            Constraint::Length(36),
            Constraint::Length(28),
            Constraint::Min(22),
            Constraint::Length(5),
        ],
        rows,
        selected,
    );
}
