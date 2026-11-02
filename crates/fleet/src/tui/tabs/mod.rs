pub mod overview;
pub mod reviews;
pub mod tmux;
pub mod worktrees;

use super::app::App;
use super::keys::Key;
use super::popups;
use super::util::{self, dim};
use fleet_core::agents::SendTarget;
use fleet_core::model::{Fleet, ReviewRow, WorktreeRow};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Tmux,
    Worktrees,
    Reviews,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Overview, Tab::Tmux, Tab::Worktrees, Tab::Reviews];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Tmux => "Tmux",
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
    pub expanded: bool,
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

#[derive(Debug, Clone)]
pub struct TabStates {
    pub overview: TabState,
    pub tmux: TabState,
    pub worktrees: TabState,
    pub reviews: TabState,
    pub show_paths: bool,
    /// Reviews source filter (`None` = all).
    pub source: Option<String>,
}

impl Default for TabStates {
    fn default() -> Self {
        Self {
            overview: TabState::default(),
            tmux: TabState {
                expanded: true,
                ..Default::default()
            },
            worktrees: TabState::default(),
            reviews: TabState::default(),
            show_paths: false,
            source: None,
        }
    }
}

impl TabStates {
    pub fn state_mut(&mut self, t: Tab) -> &mut TabState {
        match t {
            Tab::Overview => &mut self.overview,
            Tab::Tmux => &mut self.tmux,
            Tab::Worktrees => &mut self.worktrees,
            Tab::Reviews => &mut self.reviews,
        }
    }
    pub fn state(&self, t: Tab) -> &TabState {
        match t {
            Tab::Overview => &self.overview,
            Tab::Tmux => &self.tmux,
            Tab::Worktrees => &self.worktrees,
            Tab::Reviews => &self.reviews,
        }
    }
    pub fn clamp(&mut self, f: &Fleet) {
        let n = f.overview.len();
        self.overview.selected = self.overview.selected.min(n.saturating_sub(1));
        self.worktrees.selected = self
            .worktrees
            .selected
            .min(f.worktrees.len().saturating_sub(1));
        self.reviews.selected = self.reviews.selected.min(f.reviews.len().saturating_sub(1));
    }
}

/// A row key that survives tab switches: the directory (worktree path) or a review id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    Dir(std::path::PathBuf),
    Review(String, u64),
    Session(String),
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
    match app.tab {
        Tab::Overview => overview::draw(f, chunks[1], app),
        Tab::Tmux => tmux::draw(f, chunks[1], app),
        Tab::Worktrees => worktrees::draw(f, chunks[1], app),
        Tab::Reviews => reviews::draw(f, chunks[1], app),
    }
    if app.show_detail {
        let text = match app.tab {
            Tab::Overview => overview::detail(app),
            Tab::Tmux => tmux::detail(app),
            Tab::Worktrees => worktrees::detail(app),
            Tab::Reviews => reviews::detail(app),
        };
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
    spans.push(Span::raw("   "));
    let summary = match app.tab {
        Tab::Overview => format!(
            "{} worktrees · {} agents · {} need you · {} failed · {} ready",
            c.worktrees, c.agents, c.needs_you, c.failed, c.ready
        ),
        Tab::Tmux => {
            let sessions: std::collections::BTreeSet<_> = app
                .fleet
                .panes
                .iter()
                .map(|p| p.pane.session.as_str())
                .collect();
            let attached: std::collections::BTreeSet<_> = app
                .fleet
                .panes
                .iter()
                .filter(|p| p.pane.attached)
                .map(|p| p.pane.session.as_str())
                .collect();
            let oc = app
                .fleet
                .panes
                .iter()
                .filter(|p| p.pane.command == "opencode")
                .count();
            format!(
                "{} sessions · {} attached · {} opencode panes",
                sessions.len(),
                attached.len(),
                oc
            )
        }
        Tab::Worktrees => {
            let stale = app
                .fleet
                .worktrees
                .iter()
                .filter(|w| matches!(w.gc, fleet_core::model::GcVerdict::Yes(_)))
                .count();
            let unsynced = app
                .fleet
                .worktrees
                .iter()
                .filter(|w| {
                    !matches!(
                        w.worktree.sync(),
                        fleet_core::model::Sync::Synced | fleet_core::model::Sync::Detached
                    )
                })
                .count();
            let dirty = app
                .fleet
                .worktrees
                .iter()
                .filter(|w| w.worktree.dirty)
                .count();
            format!(
                "{} worktrees · {} stale · {} unsynced · {} dirty",
                c.worktrees, stale, unsynced, dirty
            )
        }
        Tab::Reviews => {
            let rows = reviews::visible(app);
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
            format!(
                "source: {} · {} open · {} failed · {} approved · filter: {}",
                app.tabs.source.as_deref().unwrap_or("all"),
                rows.len(),
                failed,
                approved,
                reviews::PRESETS[app.tabs.reviews.preset]
            )
        }
    };
    spans.push(Span::raw(summary));
    let reviews_age = match app.fleet.reviews_fetched_at {
        Some(t) => format!(" · reviews {} ago", util::ago(Some(t))),
        None if app.loading => " · loading…".to_string(),
        None => " · reviews loading…".to_string(),
    };
    spans.push(Span::styled(reviews_age, dim()));
    if !app.fleet.warnings.is_empty() {
        spans.push(Span::styled(
            format!(" · ⚠ {}", app.fleet.warnings.len()),
            Style::default().fg(Color::Red),
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
            Tab::Overview => &[
                ("Enter", "attach"),
                ("o", "open"),
                ("c", "copy"),
                ("Space", "detail"),
            ],
            Tab::Tmux => &[
                ("Enter", "attach"),
                ("h/l", "fold"),
                ("K", "kill"),
                ("n", "rename"),
            ],
            Tab::Worktrees => &[
                ("Enter", "shell"),
                ("w p", "push"),
                ("w g", "gc"),
                ("w c", "columns"),
            ],
            Tab::Reviews => &[
                ("Enter", "attach/review"),
                ("v f", "filter"),
                ("v S", "source"),
                ("S", "sort"),
            ],
        };
        for (k, l) in hints {
            spans.push(Span::styled(format!(" {k}"), util::accent()));
            spans.push(Span::styled(format!(" {l} ·"), dim()));
        }
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

// ───────────────────────── selection helpers ─────────────────────────

pub fn row_count(app: &App) -> usize {
    match app.tab {
        Tab::Overview => overview::visible(app).len(),
        Tab::Tmux => tmux::visible(app).len(),
        Tab::Worktrees => worktrees::visible(app).len(),
        Tab::Reviews => reviews::visible(app).len(),
    }
}

pub fn selected_key(app: &App) -> Option<RowKey> {
    match app.tab {
        Tab::Overview => overview::selected(app).map(|r| RowKey::Dir(r.dir.clone())),
        Tab::Tmux => tmux::selected(app).map(|r| match &r.worktree {
            Some(w) => RowKey::Dir(w.path.clone()),
            None => RowKey::Session(r.session.clone()),
        }),
        Tab::Worktrees => worktrees::selected(app).map(|w| RowKey::Dir(w.worktree.path.clone())),
        Tab::Reviews => reviews::selected(app).map(|r| {
            r.worktrees
                .first()
                .map(|w| RowKey::Dir(w.path.clone()))
                .unwrap_or(RowKey::Review(r.review.source.clone(), r.review.id))
        }),
    }
}

pub fn select_by_key(app: &mut App, key: &RowKey) {
    let idx = match app.tab {
        Tab::Overview => overview::visible(app).iter().position(|r| match key {
            RowKey::Dir(d) => r.dir == *d,
            RowKey::Review(s, id) => r.reviews.iter().any(|x| x.source == *s && x.id == *id),
            RowKey::Session(s) => r.panes.iter().any(|p| p.session == *s),
        }),
        Tab::Tmux => tmux::visible(app).iter().position(|r| match key {
            RowKey::Dir(d) => r.worktree.as_ref().map(|w| w.path == *d).unwrap_or(false),
            RowKey::Session(s) => r.session == *s,
            RowKey::Review(..) => false,
        }),
        Tab::Worktrees => worktrees::visible(app).iter().position(|w| match key {
            RowKey::Dir(d) => w.worktree.path == *d,
            RowKey::Review(s, id) => w
                .review
                .as_ref()
                .map(|r| r.source == *s && r.id == *id)
                .unwrap_or(false),
            RowKey::Session(s) => w.panes.iter().any(|p| p.session == *s),
        }),
        Tab::Reviews => reviews::visible(app).iter().position(|r| match key {
            RowKey::Dir(d) => r.worktrees.iter().any(|w| w.path == *d),
            RowKey::Review(s, id) => r.review.source == *s && r.review.id == *id,
            RowKey::Session(_) => false,
        }),
    };
    if let Some(i) = idx {
        app.tabs.state_mut(app.tab).selected = i;
    }
}

pub fn selected_pane_target(app: &App) -> Option<String> {
    match app.tab {
        Tab::Overview => overview::selected(app).and_then(|r| r.primary_pane().map(|p| p.target())),
        Tab::Tmux => tmux::selected(app).map(|r| r.target.clone()),
        Tab::Worktrees => worktrees::selected(app).and_then(|w| {
            w.panes
                .iter()
                .find(|p| p.command == "opencode")
                .or(w.panes.first())
                .map(|p| p.target())
        }),
        Tab::Reviews => reviews::selected(app).and_then(|r| {
            let dir = r.worktrees.first()?.path.clone();
            app.fleet
                .panes
                .iter()
                .find(|p| p.pane.command == "opencode" && p.pane.cwd.starts_with(&dir))
                .map(|p| p.pane.target())
        }),
    }
}

pub fn selected_plan_url(app: &App) -> Option<String> {
    let agents: Vec<fleet_core::model::AgentRow> = match app.tab {
        Tab::Overview => overview::selected(app)
            .map(|r| r.agents.clone())
            .unwrap_or_default(),
        Tab::Tmux => tmux::selected(app)
            .and_then(|r| r.agent)
            .into_iter()
            .collect(),
        Tab::Worktrees => worktrees::selected(app)
            .map(|w| w.agents.clone())
            .unwrap_or_default(),
        Tab::Reviews => reviews::selected(app)
            .map(|r| r.agents.clone())
            .unwrap_or_default(),
    };
    agents.iter().find_map(|a| a.plan_url.clone())
}

pub fn selected_review(app: &App) -> Option<ReviewRow> {
    match app.tab {
        Tab::Overview => overview::selected(app).and_then(|r| {
            r.reviews
                .iter()
                .find(|x| !x.status.is_done())
                .or(r.reviews.first())
                .cloned()
        }),
        Tab::Tmux => tmux::selected(app)
            .and_then(|r| r.worktree)
            .and_then(|w| review_for_worktree(&app.fleet, &w)),
        Tab::Worktrees => worktrees::selected(app).and_then(|w| w.review.clone()),
        Tab::Reviews => reviews::selected(app).map(|r| r.review.clone()),
    }
}

fn review_for_worktree(f: &Fleet, w: &WorktreeRow) -> Option<ReviewRow> {
    f.worktrees
        .iter()
        .find(|x| x.worktree.path == w.path)
        .and_then(|x| x.review.clone())
}

pub fn selected_worktree(app: &App) -> Option<WorktreeRow> {
    match app.tab {
        Tab::Overview => overview::selected(app).and_then(|r| r.worktree.clone()),
        Tab::Tmux => tmux::selected(app).and_then(|r| r.worktree.clone()),
        Tab::Worktrees => worktrees::selected(app).map(|w| w.worktree.clone()),
        Tab::Reviews => reviews::selected(app).and_then(|r| r.worktrees.first().cloned()),
    }
}

pub fn selected_name(app: &App) -> String {
    match app.tab {
        Tab::Overview => overview::selected(app)
            .map(|r| r.name())
            .unwrap_or_default(),
        Tab::Tmux => tmux::selected(app)
            .map(|r| r.session.clone())
            .unwrap_or_default(),
        Tab::Worktrees => worktrees::selected(app)
            .map(|w| w.worktree.name.clone())
            .unwrap_or_default(),
        Tab::Reviews => reviews::selected(app)
            .and_then(|r| r.worktrees.first().map(|w| w.name.clone()))
            .unwrap_or_default(),
    }
}

pub fn selected_send_target(app: &App) -> Option<SendTarget> {
    let (agent, pane) = match app.tab {
        Tab::Overview => overview::selected(app).map(|r| {
            (
                r.agents.first().cloned(),
                r.panes
                    .iter()
                    .find(|p| p.command == "opencode")
                    .map(|p| p.target()),
            )
        }),
        Tab::Tmux => tmux::selected(app).map(|r| {
            (
                r.agent.clone(),
                Some(r.target.clone()).filter(|_| r.command == "opencode"),
            )
        }),
        Tab::Worktrees => worktrees::selected(app).map(|w| {
            (
                w.agents.first().cloned(),
                w.panes
                    .iter()
                    .find(|p| p.command == "opencode")
                    .map(|p| p.target()),
            )
        }),
        Tab::Reviews => {
            reviews::selected(app).map(|r| (r.agents.first().cloned(), selected_pane_target(app)))
        }
    }?;
    let a = agent?;
    if let Some(p) = a.port {
        return Some(SendTarget::Port(p));
    }
    pane.map(SendTarget::Pane)
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
