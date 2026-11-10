//! Machines tab (replaces Tmux): Provider → Machine → Session (→ panes on unfold). Provider
//! rows are hidden when the machine scope pins a provider; Provider and Machine rows when it
//! pins one machine (spec: tui-machines, "Adaptive levels depend only on scope").

use super::{
    agent_cell, in_machine_scope, local_name, render_table, tree_label, MachineScope, Node,
    NodeData, RowKey, TreeNode,
};
use crate::tui::app::App;
use crate::tui::util::{self, dim, icon, meaning_style, trunc};
use fleet_core::model::{Activity, MachineView, PaneView};
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row};
use std::collections::BTreeMap;

/// The `machine` value rows on `m` carry (`None` for this computer).
pub fn row_machine(m: &MachineView) -> Option<String> {
    (m.row.provider != "local").then(|| m.row.name.clone())
}

/// Local tmux sessions, in the project scope.
fn local_sessions(app: &App) -> Vec<TreeNode> {
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
                machine: None,
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
    out.sort_by_key(|n| match &n.data {
        NodeData::Session { urgent, name, .. } => {
            (urgent.map(|a| a as u8).unwrap_or(99), name.clone())
        }
        _ => (99, String::new()),
    });
    out
}

/// A cloud machine's sessions: one per open task on it (its tmux session), with the agent
/// state the machine published.
fn remote_sessions(app: &App, m: &MachineView) -> Vec<TreeNode> {
    let mut out = vec![];
    for t in app.fleet.tasks.iter().filter(|t| t.machine_id == m.row.id) {
        let agent = app
            .fleet
            .overview
            .iter()
            .flat_map(|r| r.agents.iter())
            .find(|a| {
                a.machine.as_deref() == Some(m.row.name.as_str())
                    && a.pane_target.as_deref().and_then(|p| p.split(':').next())
                        == Some(t.tmux_session.as_str())
            })
            .cloned();
        let worktree = app
            .fleet
            .worktrees
            .iter()
            .find(|w| {
                w.worktree.machine.as_deref() == Some(m.row.name.as_str())
                    && w.worktree.path == t.path
            })
            .map(|w| w.worktree.clone());
        if let (Some(proj), Some(w)) = (&app.tabs.project, &worktree) {
            if w.repo != *proj {
                continue;
            }
        }
        out.push(TreeNode::leaf(
            RowKey::Session(format!("{}/{}", m.row.name, t.tmux_session)),
            NodeData::Session {
                machine: Some(m.row.name.clone()),
                name: t.tmux_session.clone(),
                attached: false,
                panes: vec![],
                worktree,
                agents: agent.is_some() as usize,
                urgent: agent.map(|a| a.activity),
            },
        ));
    }
    out
}

fn machine_node(app: &App, m: &MachineView) -> TreeNode {
    let children = if m.row.provider == "local" {
        local_sessions(app)
    } else {
        remote_sessions(app, m)
    };
    TreeNode::new(
        RowKey::Machine(m.row.name.clone()),
        NodeData::Machine(m.clone()),
        children,
    )
}

pub fn build(app: &App) -> Vec<TreeNode> {
    let machines: Vec<&MachineView> = app
        .fleet
        .machines
        .iter()
        .filter(|m| in_machine_scope(app, row_machine(m).as_deref()))
        .collect();
    match &app.tabs.machine {
        // One machine pinned: its sessions are the top level.
        MachineScope::Machine(_) => machines
            .first()
            .map(|m| machine_node(app, m).children)
            .unwrap_or_default(),
        MachineScope::Provider(_) => machines.iter().map(|m| machine_node(app, m)).collect(),
        MachineScope::All => {
            let mut providers: Vec<String> = vec!["local".into()];
            for m in &machines {
                if !providers.contains(&m.row.provider) {
                    providers.push(m.row.provider.clone());
                }
            }
            providers
                .into_iter()
                .map(|p| {
                    let mine: Vec<TreeNode> = machines
                        .iter()
                        .filter(|m| m.row.provider == p)
                        .map(|m| machine_node(app, m))
                        .collect();
                    let n = mine.len();
                    let running = machines
                        .iter()
                        .filter(|m| m.row.provider == p && m.row.state.is_running())
                        .count();
                    TreeNode::new(
                        RowKey::Provider(p.clone()),
                        NodeData::Provider {
                            summary: format!("{running}/{n} running"),
                            name: p,
                        },
                        mine,
                    )
                })
                .collect()
        }
    }
}

fn agent_counts(app: &App, m: &MachineView) -> Line<'static> {
    let machine = row_machine(m);
    let mut counts: BTreeMap<Activity, usize> = BTreeMap::new();
    for a in app.fleet.overview.iter().flat_map(|r| r.agents.iter()) {
        if a.machine == machine {
            *counts.entry(a.activity).or_default() += 1;
        }
    }
    let frozen = super::paused(app, machine.as_deref());
    let mut spans = vec![];
    for (a, n) in counts {
        if frozen {
            spans.push(Span::styled("⏸", meaning_style(util::Meaning::Paused)));
        }
        spans.push(Span::styled(
            format!("{}{n} ", a.glyph()),
            util::activity_style(a).add_modifier(if frozen {
                Modifier::DIM
            } else {
                Modifier::empty()
            }),
        ));
    }
    Line::from(spans)
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App, nodes: &[Node], selected: usize) {
    let home = fleet_core::config::home().to_string_lossy().into_owned();
    let local = local_name(app);
    let rows: Vec<Row> = nodes
        .iter()
        .map(|n| match &n.data {
            NodeData::Provider { name, summary } => Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(if n.unfolded { "▾ " } else { "▸ " }, dim()),
                    Span::styled(
                        format!("{} {name}", icon::machine(name)),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(Color::Cyan),
                    ),
                ])),
                Cell::from(Span::styled(summary.clone(), dim())),
            ]),
            NodeData::Machine(m) => {
                let (glyph, label, meaning) = util::machine_state(&m.row.state);
                Row::new(vec![
                    Cell::from(tree_label(
                        n,
                        &format!("{} {}", icon::machine(&m.row.provider), m.row.name),
                    )),
                    Cell::from(Span::styled(
                        format!("{glyph} {label}"),
                        meaning_style(meaning),
                    )),
                    Cell::from(m.row.size.map(|s| s.label()).unwrap_or_else(|| "—".into())),
                    Cell::from(Span::styled(
                        match (&m.row.state, m.row.idle_after) {
                            (s, Some(d)) if s.is_running() => {
                                format!("pauses after {}m idle", d.as_secs() / 60)
                            }
                            _ => "—".into(),
                        },
                        dim(),
                    )),
                    Cell::from(
                        m.row
                            .cost_usd
                            .map(|c| format!("${c:.2}"))
                            .unwrap_or_else(|| "—".into()),
                    ),
                    Cell::from(agent_counts(app, m)),
                ])
            }
            NodeData::Session {
                machine,
                name,
                attached,
                panes,
                worktree,
                urgent,
                agents,
            } => {
                let frozen = super::paused(app, machine.as_deref());
                let state = match urgent {
                    Some(a) => Line::from(vec![
                        Span::styled(
                            if frozen { "⏸ " } else { "" },
                            meaning_style(util::Meaning::Paused),
                        ),
                        Span::styled(
                            format!("{} {}", a.glyph(), a.label()),
                            util::activity_style(*a).add_modifier(if frozen {
                                Modifier::DIM
                            } else {
                                Modifier::empty()
                            }),
                        ),
                    ]),
                    None => Line::from(Span::styled("—", dim())),
                };
                Row::new(vec![
                    Cell::from(tree_label(n, &format!("{} {name}", icon::SESSION))),
                    Cell::from(state),
                    Cell::from(Span::styled(
                        {
                            let who = match agents {
                                0 => String::new(),
                                1 => " · 1 agent".into(),
                                n => format!(" · {n} agents"),
                            };
                            if machine.is_some() {
                                format!("remote{who}")
                            } else if *attached {
                                format!("attached · {} panes{who}", panes.len())
                            } else {
                                format!("detached · {} panes{who}", panes.len())
                            }
                        },
                        dim(),
                    )),
                    Cell::from(""),
                    Cell::from(Span::styled(
                        worktree
                            .as_ref()
                            .map(|w| format!("{} {}", icon::BRANCH, w.name))
                            .unwrap_or_else(|| {
                                panes
                                    .first()
                                    .map(|p| {
                                        trunc(&p.pane.cwd.to_string_lossy().replace(&home, "~"), 30)
                                    })
                                    .unwrap_or_default()
                            }),
                        dim(),
                    )),
                    Cell::from(""),
                ])
            }
            NodeData::Pane(p) => {
                let title = if p.agent.is_some() {
                    p.agent
                        .as_ref()
                        .and_then(|a| a.title.clone())
                        .unwrap_or_else(|| p.pane.title.clone())
                } else {
                    p.pane.command.clone()
                };
                Row::new(vec![
                    Cell::from(tree_label(
                        n,
                        &format!("{}:{} {}", p.pane.window, p.pane.pane, p.pane.window_name),
                    )),
                    match &p.agent {
                        Some(a) => agent_cell(a),
                        None => Cell::from(Span::styled(p.pane.command.clone(), dim())),
                    },
                    Cell::from(Span::styled(trunc(&title, 30), dim())),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                ])
            }
            _ => Row::new(vec![Cell::from("")]),
        })
        .collect();
    let _ = local;
    render_table(
        f,
        area,
        vec![
            "MACHINE / SESSION",
            "STATE",
            "SIZE",
            "IDLE",
            "COST",
            "AGENTS",
        ],
        vec![
            Constraint::Length(36),
            Constraint::Length(22),
            Constraint::Length(22),
            Constraint::Length(22),
            Constraint::Length(30),
            Constraint::Min(14),
        ],
        rows,
        selected,
    );
}
