pub mod agents;
pub mod machines;
pub mod reviews;
pub mod tree;
pub mod worktrees;

use super::app::App;
use super::keys::Key;
use super::popups;
use super::util::{self, dim};
use fleet_core::model::{Activity, Overall, ReviewRow, WorktreeRow};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::collections::HashSet;
pub use tree::{Node, NodeData, RowKey, TreeNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Agents,
    Machines,
    Worktrees,
    Reviews,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Agents, Tab::Machines, Tab::Worktrees, Tab::Reviews];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Agents => "Agents",
            Tab::Machines => "Machines",
            Tab::Worktrees => "Worktrees",
            Tab::Reviews => "Reviews",
        }
    }
    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap()
    }
    pub fn next(self) -> Tab {
        Tab::ALL[(self.index() + 1) % 4]
    }
    pub fn prev(self) -> Tab {
        Tab::ALL[(self.index() + 3) % 4]
    }
}

#[derive(Debug, Default, Clone)]
pub struct TabState {
    pub selected: usize,
    pub filter: String,
    pub unfolded: HashSet<RowKey>,
    /// Reviews: preset filter index.
    pub preset: usize,
}

impl TabState {
    pub fn up(&mut self, n: usize) {
        self.selected = self.selected.saturating_sub(n);
    }
    pub fn down(&mut self, n: usize, count: usize) {
        self.selected = (self.selected + n).min(count.saturating_sub(1));
    }
}

#[derive(Debug, Clone, Default)]
pub struct TabStates {
    pub agents: TabState,
    pub machines: TabState,
    pub worktrees: TabState,
    pub reviews: TabState,
    pub show_paths: bool,
    /// Project scope (`None` = all).
    pub project: Option<String>,
    /// Machine scope.
    pub machine: MachineScope,
}

/// Which machines the tabs show (spec: tui-machines, "Machine scope"). Cycled with
/// `machine.next` / `machine.prev` through all → each provider → all; `filter.machine` pins
/// the selected row's machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MachineScope {
    #[default]
    All,
    Provider(String),
    Machine(String),
}

impl MachineScope {
    pub fn label(&self) -> String {
        match self {
            MachineScope::All => "all".into(),
            MachineScope::Provider(p) => p.clone(),
            MachineScope::Machine(m) => m.clone(),
        }
    }
}

/// The local machine's name (`laptop` unless the profile says otherwise).
pub fn local_name(app: &App) -> String {
    app.fleet
        .machines
        .iter()
        .find(|m| m.row.provider == "local")
        .map(|m| m.row.name.clone())
        .unwrap_or_else(|| "laptop".into())
}

/// `(provider, name)` of a row's machine (`None` = this computer).
pub fn machine_of(app: &App, machine: Option<&str>) -> (String, String) {
    match machine {
        None => ("local".into(), local_name(app)),
        Some(n) => (
            app.fleet
                .machines
                .iter()
                .find(|m| m.row.name == n)
                .map(|m| m.row.provider.clone())
                .unwrap_or_else(|| "?".into()),
            n.to_string(),
        ),
    }
}

/// Is a row on `machine` inside the machine scope?
pub fn in_machine_scope(app: &App, machine: Option<&str>) -> bool {
    let (provider, name) = machine_of(app, machine);
    match &app.tabs.machine {
        MachineScope::All => true,
        MachineScope::Provider(p) => *p == provider,
        MachineScope::Machine(m) => *m == name,
    }
}

/// The machine a row is on, when it is paused (its agents are frozen).
pub fn paused(app: &App, machine: Option<&str>) -> bool {
    let Some(n) = machine else { return false };
    app.fleet.machines.iter().any(|m| {
        m.row.name == n
            && matches!(
                m.row.state,
                fleet_core::machines::MachineState::Paused { .. }
                    | fleet_core::machines::MachineState::Cooldown { .. }
                    | fleet_core::machines::MachineState::Stopped
            )
    })
}

impl TabStates {
    pub fn state_mut(&mut self, t: Tab) -> &mut TabState {
        match t {
            Tab::Agents => &mut self.agents,
            Tab::Machines => &mut self.machines,
            Tab::Worktrees => &mut self.worktrees,
            Tab::Reviews => &mut self.reviews,
        }
    }
    pub fn state(&self, t: Tab) -> &TabState {
        match t {
            Tab::Agents => &self.agents,
            Tab::Machines => &self.machines,
            Tab::Worktrees => &self.worktrees,
            Tab::Reviews => &self.reviews,
        }
    }
    /// Project sections start unfolded.
    pub fn unfold_projects(&mut self, names: &[String]) {
        for n in names {
            self.worktrees.unfolded.insert(RowKey::Project(n.clone()));
            self.reviews.unfolded.insert(RowKey::Project(n.clone()));
        }
        self.worktrees
            .unfolded
            .insert(RowKey::Project(OUTSIDE.into()));
        for n in names {
            self.agents.unfolded.insert(RowKey::Project(n.clone()));
        }
        self.agents.unfolded.insert(RowKey::Project(OUTSIDE.into()));
        for p in fleet_core::machines::PROVIDERS {
            self.machines
                .unfolded
                .insert(RowKey::Provider(p.to_string()));
        }
    }
}

pub const OUTSIDE: &str = "outside projects";

// ───────────────────────── building + navigation ─────────────────────────

/// The visible nodes of the current tab.
pub fn nodes(app: &App) -> Vec<Node> {
    let st = app.tabs.state(app.tab);
    let tree = match app.tab {
        Tab::Agents => agents::build(app),
        Tab::Machines => machines::build(app),
        Tab::Worktrees => worktrees::build(app),
        Tab::Reviews => reviews::build(app),
    };
    tree::flatten(tree, &st.unfolded, &st.filter)
}

pub fn row_count(app: &App) -> usize {
    nodes(app).len()
}

pub fn selected(app: &App) -> Option<Node> {
    nodes(app).into_iter().nth(app.tabs.state(app.tab).selected)
}

pub fn node_at(app: &App, i: usize) -> Option<Node> {
    nodes(app).into_iter().nth(i)
}

/// `l` / Right.
pub fn expand(app: &mut App) {
    let ns = nodes(app);
    let st = app.tabs.state_mut(app.tab);
    let Some(n) = ns.get(st.selected) else { return };
    if n.has_children && !n.unfolded {
        st.unfolded.insert(n.key.clone());
    } else if n.has_children {
        st.selected += 1;
    }
}

/// `h` / Left.
pub fn collapse(app: &mut App) {
    let ns = nodes(app);
    let st = app.tabs.state_mut(app.tab);
    let Some(n) = ns.get(st.selected) else { return };
    if n.has_children && n.unfolded {
        st.unfolded.remove(&n.key);
    } else if let Some(p) = tree::parent_index(&ns, st.selected) {
        st.selected = p;
    }
}

pub fn fold_all(app: &mut App) {
    let tab = app.tab;
    app.tabs.state_mut(tab).unfolded.clear();
    let sel = app.tabs.state(tab).selected;
    let ns = nodes(app);
    // keep the selection on a top-level node
    let top = (0..=sel.min(ns.len().saturating_sub(1)))
        .rev()
        .find(|&i| ns.get(i).map(|n| n.depth == 0).unwrap_or(false))
        .unwrap_or(0);
    app.tabs.state_mut(tab).selected = top;
}

pub fn unfold_all(app: &mut App) {
    let tab = app.tab;
    fn collect(t: &[TreeNode], out: &mut HashSet<RowKey>) {
        for n in t {
            if !n.children.is_empty() {
                out.insert(n.key.clone());
                collect(&n.children, out);
            }
        }
    }
    let tree = match tab {
        Tab::Agents => agents::build(app),
        Tab::Machines => machines::build(app),
        Tab::Worktrees => worktrees::build(app),
        Tab::Reviews => reviews::build(app),
    };
    let mut all = HashSet::new();
    collect(&tree, &mut all);
    app.tabs.state_mut(tab).unfolded.extend(all);
}

/// Keys of the selected node and its ancestors, for cross-tab jumps.
pub fn selected_keys(app: &App) -> Vec<RowKey> {
    let ns = nodes(app);
    let sel = app.tabs.state(app.tab).selected;
    let Some(n) = ns.get(sel) else { return vec![] };
    let mut keys = vec![n.key.clone()];
    if let Some(w) = n.worktree() {
        keys.push(RowKey::Dir(w.path.clone()));
    }
    if let Some(r) = n.review() {
        keys.push(RowKey::Review(r.source.clone(), r.id));
    }
    if let Some(t) = n.pane_target() {
        keys.push(RowKey::Pane(t.clone()));
        keys.push(RowKey::Session(
            t.split(':').next().unwrap_or(&t).to_string(),
        ));
    }
    for a in n.agents() {
        keys.push(RowKey::Agent(a.pid));
    }
    keys
}

/// Select the first node (unfolding ancestors) that relates to any of `keys`.
pub fn select_by_keys(app: &mut App, keys: &[RowKey]) {
    let tab = app.tab;
    let before = app.tabs.state(tab).unfolded.clone();
    unfold_all(app);
    let ns = nodes(app);
    let related = |n: &Node| -> bool {
        let mut mine = vec![n.key.clone()];
        if let Some(w) = n.worktree() {
            mine.push(RowKey::Dir(w.path.clone()));
        }
        if let Some(r) = n.review() {
            mine.push(RowKey::Review(r.source.clone(), r.id));
        }
        if let Some(t) = n.pane_target() {
            mine.push(RowKey::Pane(t.clone()));
            mine.push(RowKey::Session(
                t.split(':').next().unwrap_or(&t).to_string(),
            ));
        }
        for a in n.agents() {
            mine.push(RowKey::Agent(a.pid));
        }
        keys.iter().any(|k| mine.contains(k))
    };
    // Prefer the most specific match: deepest node first, then earliest.
    let best = ns
        .iter()
        .enumerate()
        .filter(|(_, n)| related(n))
        .max_by_key(|(i, n)| (n.depth, usize::MAX - i))
        .map(|(i, _)| i);
    if let Some(i) = best {
        // Re-fold everything except the ancestors of the match.
        let mut keep = HashSet::new();
        let mut j = i;
        while let Some(p) = tree::parent_index(&ns, j) {
            keep.insert(ns[p].key.clone());
            j = p;
        }
        let st = app.tabs.state_mut(tab);
        st.unfolded = keep;
        let ns2 = nodes(app);
        let st = app.tabs.state_mut(tab);
        st.selected = ns2.iter().position(|n| n.key == ns[i].key).unwrap_or(0);
    } else {
        // Nothing related here: leave the tab as the user left it.
        app.tabs.state_mut(tab).unfolded = before;
    }
}

// ───────────────────────── selection helpers used by app.rs ─────────────────────────

pub fn selected_pane_target(app: &App) -> Option<String> {
    selected(app).and_then(|n| n.pane_target())
}

pub fn selected_plan_url(app: &App) -> Option<String> {
    selected(app).and_then(|n| n.plan_url())
}

pub fn selected_review(app: &App) -> Option<ReviewRow> {
    selected(app).and_then(|n| n.review().cloned())
}

pub fn selected_worktree(app: &App) -> Option<WorktreeRow> {
    selected(app).and_then(|n| n.worktree().cloned())
}

pub fn selected_name(app: &App) -> String {
    selected(app).map(|n| n.name()).unwrap_or_default()
}

/// The first agent of the selected node, if it can receive a message.
pub fn selected_send_target(app: &App) -> Option<fleet_core::model::AgentRow> {
    let n = selected(app)?;
    let a = (*n.agents().first()?).clone();
    (a.control_url.is_some()
        || a.port.is_some()
        || a.pane_target.is_some()
        || a.session_id.is_some())
    .then_some(a)
}

// ───────────────────────── drawing ─────────────────────────

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let detail_h = if app.show_detail { 5 } else { 0 };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(detail_h),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(f, chunks[0], app);
    let ns = nodes(app);
    let sel = app
        .tabs
        .state(app.tab)
        .selected
        .min(ns.len().saturating_sub(1));
    match app.tab {
        Tab::Agents => agents::draw(f, chunks[1], app, &ns, sel),
        Tab::Machines => machines::draw(f, chunks[1], app, &ns, sel),
        Tab::Worktrees => worktrees::draw(f, chunks[1], app, &ns, sel),
        Tab::Reviews => reviews::draw(f, chunks[1], app, &ns, sel),
    }
    if app.show_detail {
        let text = ns.get(sel).map(|n| detail(app, n)).unwrap_or_default();
        let p = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::TOP).border_style(dim()));
        f.render_widget(p, chunks[2]);
    }
    draw_footer(f, chunks[3], app);

    if let Some(menu) = &app.menu {
        popups::draw_whichkey(f, area, &app.pending, menu);
    }
    if let Some(p) = &app.popup {
        popups::draw(f, area, p, app);
    }
}

/// Detail panel for any node kind (shared by all tabs).
pub fn detail(app: &App, n: &Node) -> Text<'static> {
    let home = fleet_core::config::home().to_string_lossy().into_owned();
    let tilde = |s: &str| s.replace(&home, "~");
    let mut lines = vec![];
    match &n.data {
        NodeData::Project { name, summary } => {
            lines.push(Line::from(vec![
                Span::styled(format!(" {name} "), util::accent()),
                Span::raw(summary.clone()),
            ]));
            if let Some(p) = app.cfg.project(name) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "   repo {} · new worktrees in {} · reviews {} · base {}",
                        tilde(&p.repo.display().to_string()),
                        tilde(&p.worktrees_dir().display().to_string()),
                        p.reviews.as_deref().unwrap_or("none"),
                        p.base
                    ),
                    dim(),
                )));
            }
        }
        NodeData::Agent {
            agent,
            worktree,
            review,
            pane,
            project,
        } => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {} ", agent.activity.glyph(), agent.activity.label()),
                    util::activity_style(agent.activity),
                ),
                Span::raw(format!(
                    "{} · {} · {}",
                    agent.mode.as_deref().unwrap_or("?"),
                    agent.model.as_deref().unwrap_or("model ?"),
                    agent.title.as_deref().unwrap_or("(no title)")
                )),
                Span::styled(
                    format!(
                        " · {} · {} ago{}{}",
                        pane.as_ref()
                            .map(|p| p.target())
                            .or(agent.pane_target.clone())
                            .unwrap_or("no tmux".into()),
                        util::ago(agent.last_activity),
                        agent
                            .port
                            .map(|p| format!(" · port {p}"))
                            .unwrap_or_default(),
                        agent
                            .session_id
                            .as_ref()
                            .map(|s| format!(" · {s}"))
                            .unwrap_or_default()
                    ),
                    dim(),
                ),
            ]));
            if let Some(u) = &agent.plan_url {
                lines.push(Line::from(vec![
                    Span::raw("   plan: "),
                    Span::styled(u.clone(), Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("  {}", agent.plan_title.as_deref().unwrap_or("")),
                        dim(),
                    ),
                ]));
            }
            for pnd in &agent.pending {
                lines.push(Line::from(vec![
                    Span::raw("   permission: "),
                    Span::styled(pnd.clone(), Style::default().fg(Color::Yellow)),
                ]));
            }
            if let Some(t) = &agent.running_tool {
                if agent.activity == Activity::Working {
                    lines.push(Line::from(Span::styled(
                        format!("   running tool: {t}"),
                        dim(),
                    )));
                }
            }
            match worktree {
                Some(w) => lines.push(Line::from(format!(
                    "   worktree {} ({}) · {} {}{} · {}",
                    w.name,
                    project.as_deref().unwrap_or("?"),
                    w.branch.as_deref().unwrap_or("(detached)"),
                    w.sync_glyph(),
                    if w.dirty { " *" } else { "" },
                    tilde(&w.path.display().to_string())
                ))),
                None => lines.push(Line::from(Span::styled(
                    format!(
                        "   {} (not a worktree of a configured project)",
                        tilde(&agent.cwd.display().to_string())
                    ),
                    dim(),
                ))),
            }
            if let Some(r) = review {
                lines.push(review_line(r));
            }
        }
        NodeData::Worktree(v) => {
            let w = &v.worktree;
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", v.overall.label()),
                    util::overall_style(v.overall),
                ),
                Span::styled(format!("{} ", w.name), util::accent()),
                Span::raw(format!(
                    "{} {}{} · upstream {} · HEAD {}",
                    w.branch.as_deref().unwrap_or("(detached)"),
                    w.sync_glyph(),
                    if w.dirty { " · dirty" } else { " · clean" },
                    w.upstream.as_deref().unwrap_or("none"),
                    w.head
                )),
            ]));
            let sessions: std::collections::BTreeSet<_> =
                v.panes.iter().map(|p| p.session.clone()).collect();
            lines.push(Line::from(Span::styled(
                format!(
                    "   {} · tmux {} · gc {}",
                    tilde(&w.path.display().to_string()),
                    if sessions.is_empty() {
                        "none".into()
                    } else {
                        sessions.into_iter().collect::<Vec<_>>().join(", ")
                    },
                    v.gc.label()
                ),
                dim(),
            )));
            for a in &v.agents {
                lines.push(agent_line(a));
            }
            if let Some(r) = &v.review {
                lines.push(review_line(r));
            }
        }
        NodeData::Session {
            name,
            attached,
            panes,
            worktree,
            ..
        } => {
            lines.push(Line::from(vec![
                Span::styled(format!(" {name} "), util::accent()),
                Span::raw(format!(
                    "{} · {} panes · {}",
                    if *attached { "attached" } else { "detached" },
                    panes.len(),
                    worktree
                        .as_ref()
                        .map(|w| format!("worktree {}", w.name))
                        .unwrap_or_else(|| panes
                            .first()
                            .map(|p| tilde(&p.pane.cwd.display().to_string()))
                            .unwrap_or_default())
                )),
            ]));
            for p in panes {
                if let Some(a) = &p.agent {
                    lines.push(agent_line(a));
                }
            }
        }
        NodeData::Pane(p) => {
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", p.pane.target()), util::accent()),
                Span::raw(format!(
                    "{} · {} · {}",
                    p.pane.command,
                    p.pane.title,
                    tilde(&p.pane.cwd.display().to_string())
                )),
            ]));
            if let Some(w) = &p.worktree {
                lines.push(Line::from(format!(
                    "   worktree {} · {} {}",
                    w.name,
                    w.branch.as_deref().unwrap_or("(detached)"),
                    w.sync_glyph()
                )));
            }
            if let Some(a) = &p.agent {
                lines.push(agent_line(a));
                if let Some(u) = &a.plan_url {
                    lines.push(Line::from(vec![
                        Span::raw("     plan: "),
                        Span::styled(u.clone(), Style::default().fg(Color::Yellow)),
                    ]));
                }
            }
        }
        NodeData::Provider { name, summary } => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {name} ", util::icon::machine(name)),
                    util::accent(),
                ),
                Span::raw(summary.clone()),
            ]));
            let profiles: Vec<String> = app
                .cfg
                .machine_profiles()
                .into_iter()
                .filter(|p| p.provider == *name)
                .map(|p| p.name)
                .collect();
            lines.push(Line::from(Span::styled(
                format!("   profiles: {}", profiles.join(", ")),
                dim(),
            )));
        }
        NodeData::Machine(m) => {
            let r = &m.row;
            let (glyph, label, meaning) = util::machine_state(&r.state);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {} ", util::icon::machine(&r.provider), r.name),
                    util::accent(),
                ),
                Span::styled(format!("{glyph} {label}"), util::meaning_style(meaning)),
                Span::raw(format!(
                    " · {} · profile {} · id {}",
                    r.provider, m.profile, r.id
                )),
            ]));
            let fmt = |d: Option<std::time::Duration>| {
                d.map(|d| format!("{}m", d.as_secs() / 60))
                    .unwrap_or_else(|| "off".into())
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "   size {} · idle pause {} · max run {} · cost {} · OOM kills {} · tasks {}",
                    r.size.map(|s| s.label()).unwrap_or_else(|| "—".into()),
                    fmt(r.idle_after),
                    fmt(r.max_run),
                    r.cost_usd
                        .map(|c| format!("${c:.2}"))
                        .unwrap_or_else(|| "—".into()),
                    r.oom_kills
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "—".into()),
                    if m.tasks.is_empty() {
                        "none".into()
                    } else {
                        m.tasks.join(", ")
                    }
                ),
                dim(),
            )));
            if let fleet_core::machines::MachineState::Error { reason } = &r.state {
                lines.push(Line::from(Span::styled(
                    format!("   error: {reason}"),
                    util::meaning_style(util::Meaning::Failed),
                )));
            }
        }
        NodeData::Review(v) => {
            let r = &v.review;
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", r.display_id), util::accent()),
                Span::raw(format!(
                    "{} · by {} · {} → {} · updated {} ago",
                    util::trunc(&r.title, 80),
                    r.author,
                    r.source_branch,
                    r.target_branch,
                    util::ago(r.updated_at)
                )),
            ]));
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(r.status.label().to_string(), util::review_style(r.status)),
                Span::raw(format!(
                    " · CI {} · approved by {} · {} comments: {} resolved, {} open · ",
                    r.ci.glyph(),
                    if r.approvals.is_empty() {
                        "nobody".into()
                    } else {
                        r.approvals.join(", ")
                    },
                    r.threads.total,
                    r.threads.resolved,
                    r.threads.unresolved
                )),
                Span::styled(r.url.clone(), dim()),
            ]));
            if v.worktrees.is_empty() {
                lines.push(Line::from(Span::styled(
                    "   no local worktree · Enter / v r starts a review agent",
                    dim(),
                )));
            } else if !n.unfolded {
                lines.push(Line::from(Span::styled(
                    format!(
                        "   {} local worktree(s), {} agent(s) · l unfolds",
                        v.worktrees.len(),
                        v.agents.len()
                    ),
                    dim(),
                )));
            }
        }
    }
    Text::from(lines)
}

fn agent_line(a: &fleet_core::model::AgentRow) -> Line<'static> {
    let perm = a
        .pending
        .first()
        .map(|p| format!(" · permission: {p}"))
        .unwrap_or_default();
    Line::from(vec![
        Span::raw("   "),
        Span::styled(
            format!("{} {}", a.activity.glyph(), a.activity.label()),
            util::activity_style(a.activity),
        ),
        Span::raw(format!(
            " {}{} · {}",
            a.mode.as_deref().unwrap_or("?"),
            a.model_short()
                .map(|m| format!(" · {m}"))
                .unwrap_or_default(),
            util::trunc(a.title.as_deref().unwrap_or("(no title)"), 70)
        )),
        Span::styled(
            format!(
                " · {} · {} ago",
                a.pane_target.as_deref().unwrap_or("no tmux"),
                util::ago(a.last_activity)
            ),
            dim(),
        ),
        Span::styled(perm, Style::default().fg(Color::Yellow)),
    ])
}

fn review_line(r: &ReviewRow) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("   {} ", r.display_id)),
        Span::styled(r.status.label().to_string(), util::review_style(r.status)),
        Span::raw(format!(
            " · CI {} · 👍{} · {} comments, {} open · ",
            r.ci.glyph(),
            r.approvals.len(),
            r.threads.total,
            r.threads.unresolved
        )),
        Span::styled(r.url.clone(), dim()),
    ])
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let c = app.fleet.counts();
    let mut spans = vec![Span::styled(" fleet ", util::accent())];
    for t in Tab::ALL {
        let label = format!("{} {}", t.index() + 1, t.title());
        if t == app.tab {
            spans.push(Span::styled(
                format!("[{label}]"),
                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ));
        } else {
            spans.push(Span::styled(format!(" {label} "), dim()));
        }
    }
    spans.push(Span::styled(
        format!(
            "  project: {}  machine: {} ▸ ",
            app.tabs.project.as_deref().unwrap_or("all"),
            app.tabs.machine.label()
        ),
        Style::default().fg(Color::Cyan),
    ));
    let summary = match app.tab {
        Tab::Agents => {
            let ns = agents::build(app);
            let waiting = ns.iter().filter(|n| matches!(&n.data, NodeData::Agent { agent, .. } if agent.activity.is_waiting())).count();
            let working = ns.iter().filter(|n| matches!(&n.data, NodeData::Agent { agent, .. } if agent.activity == Activity::Working)).count();
            format!(
                "{} agents · {} need you · {} working · {} idle",
                ns.len(),
                waiting,
                working,
                ns.len().saturating_sub(waiting + working)
            )
        }
        Tab::Machines => {
            let ms: Vec<_> = app
                .fleet
                .machines
                .iter()
                .filter(|m| in_machine_scope(app, machines::row_machine(m).as_deref()))
                .collect();
            let running = ms.iter().filter(|m| m.row.state.is_running()).count();
            let costs: Vec<f64> = ms.iter().filter_map(|m| m.row.cost_usd).collect();
            let cost = if costs.is_empty() {
                "no cost data".to_string()
            } else {
                format!("${:.2} this month", costs.iter().sum::<f64>().max(0.0))
            };
            format!(
                "{} machines · {} running · {} paused/stopped · {cost}",
                ms.len(),
                running,
                ms.len() - running
            )
        }
        Tab::Worktrees => {
            let wts = worktrees::scoped(app);
            let needs = wts
                .iter()
                .filter(|w| w.overall == Overall::NeedsYou)
                .count();
            let failed = wts.iter().filter(|w| w.overall == Overall::Failed).count();
            let ready = wts.iter().filter(|w| w.overall == Overall::Ready).count();
            let stale = wts
                .iter()
                .filter(|w| matches!(w.gc, fleet_core::model::GcVerdict::Yes(_)))
                .count();
            format!(
                "{} worktrees · {} need you · {} failed · {} ready · {} stale",
                wts.len(),
                needs,
                failed,
                ready,
                stale
            )
        }
        Tab::Reviews => {
            let rows = reviews::scoped(app);
            let failed = rows
                .iter()
                .filter(|r| {
                    matches!(
                        r.review.status,
                        fleet_core::model::ReviewStatus::CiFailed
                            | fleet_core::model::ReviewStatus::Conflicts
                    )
                })
                .count();
            let approved = rows
                .iter()
                .filter(|r| !r.review.approvals.is_empty())
                .count();
            let local = rows.iter().filter(|r| !r.worktrees.is_empty()).count();
            format!(
                "{} open · {} local · {} failed · {} approved · filter: {}",
                rows.len(),
                local,
                failed,
                approved,
                reviews::PRESETS[app.tabs.reviews.preset]
            )
        }
    };
    spans.push(Span::raw(summary));
    let _ = c;
    let reviews_age = match app.fleet.reviews_fetched_at {
        Some(t) => format!(" · reviews {} ago", util::ago(Some(t))),
        None if app.loading => " · loading…".to_string(),
        None => " · reviews loading…".to_string(),
    };
    spans.push(Span::styled(reviews_age, dim()));
    if let Some(w) = app.fleet.warnings.first() {
        // Show the first warning's text, not just a count: a failed reviews fetch would
        // otherwise look like a project with no open reviews.
        let more = app.fleet.warnings.len() - 1;
        let text = if more > 0 {
            format!(" · ⚠ {w} (+{more})")
        } else {
            format!(" · ⚠ {w}")
        };
        spans.push(Span::styled(text, Style::default().fg(Color::Red)));
    }
    if util::newer_binary_installed(app.started_exe) {
        spans.push(Span::styled(
            " · restart: newer fleet installed",
            Style::default().fg(Color::Yellow),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = if let Some((s, err)) = &app.status_line {
        Line::from(Span::styled(
            format!(" {s}"),
            if *err {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            },
        ))
    } else if !app.pending.is_empty() {
        Line::from(Span::styled(
            format!(
                " {} …",
                app.pending
                    .iter()
                    .map(Key::display)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            util::accent(),
        ))
    } else {
        let st = app.tabs.state(app.tab);
        let mut spans = vec![];
        let hints: &[(&str, &str)] = match app.tab {
            Tab::Agents => &[
                ("Enter", "attach"),
                ("o", "open plan/review"),
                ("c", "copy"),
                ("a m", "message"),
            ],
            Tab::Machines => &[
                ("Enter", "attach / shell"),
                ("l/h", "unfold/fold"),
                ("] m", "machine scope"),
                ("Space", "pause · resume · keep · …"),
            ],
            Tab::Worktrees => &[
                ("Enter", "attach / shell"),
                ("l/h", "unfold/fold"),
                ("w x", "close task"),
                ("w g", "gc"),
            ],
            Tab::Reviews => &[
                ("Enter", "unfold (local) / review (none)"),
                ("v r", "review"),
                ("v f", "filter"),
                ("a a", "attach agent"),
            ],
        };
        for (k, l) in hints {
            spans.push(Span::styled(format!(" {k}"), util::accent()));
            spans.push(Span::styled(format!(" {l} ·"), dim()));
        }
        spans.push(Span::styled(" p", util::accent()));
        spans.push(Span::styled(" project ·", dim()));
        for (k, label) in app.keymap.prefixes() {
            spans.push(Span::styled(format!(" {}", k.display()), util::accent()));
            spans.push(Span::styled(format!(" {label}"), dim()));
        }
        spans.push(Span::styled(" · / filter · ? help", dim()));
        if !st.filter.is_empty() {
            spans.push(Span::styled(
                format!("   filter: {}", st.filter),
                Style::default().fg(Color::Yellow),
            ));
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

/// Shared table helper: header + rows with selection highlight and scrolling.
pub fn render_table(
    f: &mut Frame,
    area: Rect,
    header: Vec<&str>,
    widths: Vec<Constraint>,
    rows: Vec<ratatui::widgets::Row<'static>>,
    selected: usize,
) {
    use ratatui::widgets::{Row, Table, TableState};
    let header = Row::new(
        header
            .into_iter()
            .map(|h| Span::styled(h.to_string(), dim().add_modifier(Modifier::BOLD)))
            .collect::<Vec<_>>(),
    );
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸");
    let mut st = TableState::default().with_selected(Some(selected));
    f.render_stateful_widget(table, area, &mut st);
}

/// `"  ▾ name"` / `"  ▸ name"` / `"    name"` for tree rows.
pub fn tree_label(n: &Node, name: &str) -> Line<'static> {
    let indent = "  ".repeat(n.depth as usize);
    let glyph = if n.has_children {
        if n.unfolded {
            "▾ "
        } else {
            "▸ "
        }
    } else if n.depth > 0 {
        "↳ "
    } else {
        "  " // same width as the fold glyph, so names align across parent and leaf rows
    };
    Line::from(vec![
        Span::styled(format!("{indent}{glyph}"), dim()),
        Span::raw(name.to_string()),
    ])
}

/// Compact agent cell used by every tab: `plan  ◆ WAIT:plan`.
pub fn agent_cell(a: &fleet_core::model::AgentRow) -> ratatui::widgets::Cell<'static> {
    ratatui::widgets::Cell::from(Line::from(vec![
        Span::styled(format!("{:<5} ", a.mode.as_deref().unwrap_or("?")), dim()),
        Span::styled(
            format!("{} {}", a.activity.glyph(), a.activity.label()),
            util::activity_style(a.activity),
        ),
    ]))
}

/// Compact review cell: `!514 ci failed ✗ 👍0 💬7 ✘4`.
pub fn review_cell(r: Option<&ReviewRow>) -> ratatui::widgets::Cell<'static> {
    use ratatui::widgets::Cell;
    match r {
        None => Cell::from(Span::styled("—", dim())),
        Some(r) => Cell::from(Line::from(vec![
            Span::raw(format!("{} ", r.display_id)),
            Span::styled(r.status.label().to_string(), util::review_style(r.status)),
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

/// `<icon> <machine>` (laptop or cloud type icon). Blue while the machine is paused.
pub fn machine_spans(app: &App, machine: Option<&str>) -> Vec<Span<'static>> {
    let (provider, name) = machine_of(app, machine);
    let style = if paused(app, machine) {
        util::meaning_style(util::Meaning::Paused)
    } else {
        util::accent()
    };
    vec![Span::styled(
        format!(
            "{} {}",
            util::icon::machine(&provider),
            util::trunc(&name, 16)
        ),
        style,
    )]
}

pub fn machine_cell(app: &App, machine: Option<&str>) -> ratatui::widgets::Cell<'static> {
    ratatui::widgets::Cell::from(Line::from(machine_spans(app, machine)))
}

/// WHERE: `<icon> <machine> › <session>`, then `●` attached / `○` detached.
pub fn where_cell(
    app: &App,
    machine: Option<&str>,
    target: Option<&str>,
    attached: bool,
) -> ratatui::widgets::Cell<'static> {
    use ratatui::widgets::Cell;
    let mut spans = machine_spans(app, machine);
    match target {
        None => spans.push(Span::styled(" › —", dim())),
        Some(t) => {
            let session = t.split(':').next().unwrap_or(t);
            spans.push(Span::styled(" › ", dim()));
            spans.push(Span::raw(util::trunc(session, 18)));
            spans.push(Span::styled(
                if attached { " ●" } else { " ○" },
                if attached {
                    Style::default().fg(Color::Green)
                } else {
                    dim()
                },
            ));
        }
    }
    Cell::from(Line::from(spans))
}

/// An agent's STATE: `◆ WAIT:plan`; on a paused machine `⏸` (blue) then the last-known
/// state dimmed (`⏸◆ WAIT:plan`).
pub fn state_spans(app: &App, a: &fleet_core::model::AgentRow) -> Vec<Span<'static>> {
    let text = format!("{} {}", a.activity.glyph(), a.activity.label());
    if paused(app, a.machine.as_deref()) {
        vec![
            Span::styled("⏸", util::meaning_style(util::Meaning::Paused)),
            Span::styled(
                text,
                util::activity_style(a.activity).add_modifier(Modifier::DIM),
            ),
        ]
    } else {
        vec![Span::styled(text, util::activity_style(a.activity))]
    }
}

pub fn state_cell(app: &App, a: &fleet_core::model::AgentRow) -> ratatui::widgets::Cell<'static> {
    ratatui::widgets::Cell::from(Line::from(state_spans(app, a)))
}

/// The fleet task an agent belongs to: the open task whose tmux session hosts its pane, else
/// whose worktree contains its directory (on the same machine).
pub fn task_of(app: &App, a: &fleet_core::model::AgentRow) -> Option<String> {
    let session = a
        .pane_target
        .as_deref()
        .map(|t| t.split(':').next().unwrap_or(t));
    let same_machine = |t: &fleet_core::model::TaskBrief| match &a.machine {
        None => !app
            .fleet
            .machines
            .iter()
            .any(|m| m.row.id == t.machine_id && m.row.provider != "local"),
        Some(n) => app
            .fleet
            .machines
            .iter()
            .any(|m| m.row.id == t.machine_id && m.row.name == *n),
    };
    let tasks = || app.fleet.tasks.iter().filter(|t| same_machine(t));
    tasks()
        .find(|t| Some(t.tmux_session.as_str()) == session)
        .or_else(|| tasks().find(|t| a.cwd.starts_with(&t.path)))
        .map(|t| t.slug.clone())
}

/// A worktree's STATE in the shared vocabulary: `◆ NEEDS YOU`, `✓ READY`, `● WORKING`, …
pub fn overall_cell(
    o: Overall,
    gc: &fleet_core::model::GcVerdict,
) -> ratatui::widgets::Cell<'static> {
    let glyph = match o {
        Overall::NeedsYou => "◆",
        Overall::Failed => "✕",
        Overall::Ready | Overall::Merged => "✓",
        Overall::Working => "●",
        Overall::Idle => "○",
        Overall::Dead => "■",
    };
    let mut spans = vec![Span::styled(
        format!("{glyph} {}", o.label()),
        util::overall_style(o),
    )];
    if let fleet_core::model::GcVerdict::Yes(r) = gc {
        spans.push(Span::styled(
            format!(" · gc: {r}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    ratatui::widgets::Cell::from(Line::from(spans))
}
