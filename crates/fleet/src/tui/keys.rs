//! Actions and the keymap that drives them. Every key comes from the effective config
//! (`config/default/keys/mod.lua` merged with the user's `keys`, per project); nothing here
//! hard-codes a chord.
//!
//! Layout: Vim motions are global single keys or short chords (`j`, `g g`, `C-d`). `Space` is
//! the **leader**: it opens a menu for the selected row showing only what applies to it (an
//! agent row: attach / message / stop / …; a worktree row: shell / close / push / …). Under
//! the leader, `tasks` (default `t`) opens the tasks menu, where each task's own `key` starts
//! it. A key value that is a chord (`"a s"`) or a modified key (`C-n`) is a full top-level
//! binding, so old-style overrides keep working.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fleet_core::config::Project;
use fleet_core::Config;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Help,
    Refresh,
    RefreshReviews,
    Filter,
    FindNext,
    FindPrev,
    FilterProject,
    Command,
    Inspect,
    Up,
    Down,
    Top,
    Bottom,
    PageUp,
    PageDown,
    Tab1,
    Tab2,
    Tab3,
    Tab4,
    NextTab,
    PrevTab,
    Primary,
    Open,
    Yank,
    Collapse,
    Expand,
    Sort,
    GotoAgents,
    GotoMachines,
    GotoWorktree,
    GotoReview,
    ProjectNext,
    ProjectPrev,
    FoldAll,
    UnfoldAll,
    // leader menu, by row
    AgentAttach,
    AgentSend,
    AgentRename,
    AgentStop,
    AgentOpenPlan,
    AgentCopyPlan,
    WorktreeShell,
    WorktreePush,
    WorktreePull,
    WorktreeClose,
    WorktreeGc,
    WorktreeColumns,
    WorktreeOpenReview,
    ReviewOpen,
    ReviewCopy,
    ReviewStart,
    ReviewGotoWorktrees,
    ReviewGotoAgents,
    ReviewFilter,
    TmuxKill,
    TmuxRename,
    AgentDiff,
    MachineNext,
    MachinePrev,
    FilterMachine,
    MachineShell,
    MachinePause,
    MachineResume,
    MachineKeep,
    MachineIdle,
    MachineStopStart,
    MachineDelete,
    /// A task from config (`tasks.<name>`), started on the selected row.
    Task(String),
}

/// What kind of row is selected: decides which leader-menu entries apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowKind {
    Agent,
    Worktree,
    Review,
    Tmux,
    Machine,
    /// A project section or nothing.
    Other,
}

/// Where a leader-menu action applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Global,
    Agent,
    /// Worktree rows, and agent rows (an agent lives in a worktree).
    Worktree,
    Review,
    Tmux,
    Machine,
    /// Under the leader on every row.
    Any,
}

impl Scope {
    fn applies(self, k: RowKind) -> bool {
        match self {
            Scope::Global => false,
            Scope::Any => true,
            Scope::Agent => k == RowKind::Agent,
            Scope::Worktree => matches!(k, RowKind::Worktree | RowKind::Agent),
            Scope::Review => k == RowKind::Review,
            Scope::Tmux => k == RowKind::Tmux,
            Scope::Machine => k == RowKind::Machine,
        }
    }
}

impl Action {
    /// Every built-in action (tasks come from config).
    pub const ALL: &'static [Action] = &[
        Action::Quit,
        Action::Help,
        Action::Refresh,
        Action::RefreshReviews,
        Action::Filter,
        Action::FindNext,
        Action::FindPrev,
        Action::FilterProject,
        Action::Command,
        Action::Inspect,
        Action::Up,
        Action::Down,
        Action::Top,
        Action::Bottom,
        Action::PageUp,
        Action::PageDown,
        Action::Tab1,
        Action::Tab2,
        Action::Tab3,
        Action::Tab4,
        Action::NextTab,
        Action::PrevTab,
        Action::Primary,
        Action::Open,
        Action::Yank,
        Action::Collapse,
        Action::Expand,
        Action::Sort,
        Action::GotoAgents,
        Action::GotoMachines,
        Action::GotoWorktree,
        Action::GotoReview,
        Action::ProjectNext,
        Action::ProjectPrev,
        Action::FoldAll,
        Action::UnfoldAll,
        Action::AgentAttach,
        Action::AgentSend,
        Action::AgentRename,
        Action::AgentStop,
        Action::AgentOpenPlan,
        Action::AgentCopyPlan,
        Action::WorktreeShell,
        Action::WorktreePush,
        Action::WorktreePull,
        Action::WorktreeClose,
        Action::WorktreeGc,
        Action::WorktreeColumns,
        Action::WorktreeOpenReview,
        Action::ReviewOpen,
        Action::ReviewCopy,
        Action::ReviewStart,
        Action::ReviewGotoWorktrees,
        Action::ReviewGotoAgents,
        Action::ReviewFilter,
        Action::TmuxKill,
        Action::TmuxRename,
        Action::AgentDiff,
        Action::MachineNext,
        Action::MachinePrev,
        Action::FilterMachine,
        Action::MachineShell,
        Action::MachinePause,
        Action::MachineResume,
        Action::MachineKeep,
        Action::MachineIdle,
        Action::MachineStopStart,
        Action::MachineDelete,
    ];

    /// `(config name, label, scope)`. No default chord: the shipped config carries them.
    fn spec(&self) -> (&'static str, &'static str, Scope) {
        use Scope::*;
        match self {
            Action::Quit => ("quit", "quit", Global),
            Action::Help => ("help", "help / cheat sheet", Global),
            Action::Refresh => ("refresh", "refresh now", Global),
            Action::RefreshReviews => ("refresh.reviews", "refresh reviews now", Global),
            Action::Filter => ("filter", "filter rows", Global),
            Action::FindNext => ("find.next", "next match", Global),
            Action::FindPrev => ("find.prev", "previous match", Global),
            Action::FilterProject => ("filter.project", "scope to this row's project", Global),
            Action::Command => ("command", "command line", Global),
            Action::Inspect => ("inspect", "toggle detail panel", Global),
            Action::Up => ("up", "up", Global),
            Action::Down => ("down", "down", Global),
            Action::Top => ("top", "top", Global),
            Action::Bottom => ("bottom", "bottom", Global),
            Action::PageUp => ("page.up", "page up", Global),
            Action::PageDown => ("page.down", "page down", Global),
            Action::Tab1 => ("tab.agents", "Agents tab", Global),
            Action::Tab2 => ("tab.machines", "Machines tab", Global),
            Action::Tab3 => ("tab.worktrees", "Worktrees tab", Global),
            Action::Tab4 => ("tab.reviews", "Reviews tab", Global),
            Action::NextTab => ("tab.next", "next tab", Global),
            Action::PrevTab => ("tab.prev", "previous tab", Global),
            Action::Primary => ("primary", "attach / shell / review", Global),
            Action::Open => ("open", "open plan or review in browser", Global),
            Action::Yank => ("yank", "copy URL / id", Global),
            Action::Collapse => ("collapse", "collapse", Global),
            Action::Expand => ("expand", "expand", Global),
            Action::Sort => ("sort", "cycle sort", Global),
            Action::GotoAgents => ("goto.agents", "this row in Agents", Global),
            Action::GotoMachines => ("goto.machines", "this row in Machines", Global),
            Action::GotoWorktree => ("goto.worktree", "this row in Worktrees", Global),
            Action::GotoReview => ("goto.review", "this row in Reviews", Global),
            Action::ProjectNext => ("project.next", "next project (… → all)", Global),
            Action::ProjectPrev => ("project.prev", "previous project", Global),
            Action::FoldAll => ("fold.all", "fold all", Global),
            Action::UnfoldAll => ("unfold.all", "unfold all", Global),
            Action::AgentAttach => ("agent.attach", "attach (tmux)", Agent),
            Action::AgentSend => ("agent.send", "send message", Agent),
            Action::AgentRename => ("agent.rename", "rename task", Agent),
            Action::AgentStop => ("agent.stop", "stop agent (confirm)", Agent),
            Action::AgentOpenPlan => ("agent.open_plan", "open plan URL", Agent),
            Action::AgentCopyPlan => ("agent.copy_plan", "copy plan URL", Agent),
            Action::WorktreeShell => ("worktree.shell", "shell here (tmux window)", Worktree),
            Action::WorktreePush => ("worktree.push", "push -u", Worktree),
            Action::WorktreePull => ("worktree.pull", "pull --ff-only", Worktree),
            Action::WorktreeClose => (
                "worktree.close",
                "close task: everything (confirm)",
                Worktree,
            ),
            Action::WorktreeGc => ("worktree.gc", "gc (dry run)", Any),
            Action::WorktreeColumns => ("worktree.columns", "toggle name/path columns", Any),
            Action::WorktreeOpenReview => ("worktree.open_review", "open its review", Worktree),
            Action::ReviewOpen => ("review.open", "open in browser", Review),
            Action::ReviewCopy => ("review.copy", "copy URL", Review),
            Action::ReviewStart => ("review.start", "review / re-review (agent)", Review),
            Action::ReviewGotoWorktrees => ("review.worktrees", "its worktrees", Review),
            Action::ReviewGotoAgents => ("review.agents", "its agents", Review),
            Action::ReviewFilter => (
                "review.filter",
                "filter: all/mine/others/needs-me/failed",
                Review,
            ),
            Action::TmuxKill => ("tmux.kill", "kill session (confirm)", Tmux),
            Action::TmuxRename => ("tmux.rename", "rename session", Tmux),
            Action::AgentDiff => ("agent.diff", "diff vs base in nvim (its machine)", Agent),
            Action::MachineNext => (
                "machine.next",
                "next machine scope (all → providers)",
                Global,
            ),
            Action::MachinePrev => ("machine.prev", "previous machine scope", Global),
            Action::FilterMachine => ("filter.machine", "scope to this row's machine", Global),
            Action::MachineShell => ("machine.shell", "shell on the machine (wakes it)", Machine),
            Action::MachinePause => ("machine.pause", "pause (memory kept)", Machine),
            Action::MachineResume => ("machine.resume", "resume / wake", Machine),
            Action::MachineKeep => ("machine.keep", "keep awake 1h", Machine),
            Action::MachineIdle => ("machine.idle", "idle pause: 10m → 30m → off", Machine),
            Action::MachineStopStart => ("machine.stop", "stop / start", Machine),
            Action::MachineDelete => ("machine.delete", "delete machine (confirm)", Machine),
            Action::Task(_) => ("task", "task", Any),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Action::Task(n) => format!("task.{n}"),
            a => a.spec().0.to_string(),
        }
    }

    pub fn label(&self) -> &'static str {
        self.spec().1
    }
}

/// One key press, normalised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.display().cmp(&other.display())
    }
}

impl Key {
    pub fn from_event(e: &KeyEvent) -> Self {
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let mut shift = e.modifiers.contains(KeyModifiers::SHIFT);
        let code = match e.code {
            KeyCode::Char(c) => {
                // Shift is implied by the uppercase char; keep `shift` only for non-chars.
                shift = false;
                KeyCode::Char(c)
            }
            KeyCode::BackTab => {
                shift = true;
                KeyCode::Tab
            }
            other => other,
        };
        Key { code, ctrl, shift }
    }

    /// `C-u`, `S-Tab`, `Enter`, `Space`, `g`, `G`, `?`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("C-") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("S-") {
                shift = true;
                rest = r;
            } else {
                break;
            }
        }
        let code = match rest {
            "Enter" | "enter" => KeyCode::Enter,
            "Space" | "space" => KeyCode::Char(' '),
            "Tab" | "tab" => KeyCode::Tab,
            "Esc" | "esc" => KeyCode::Esc,
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "Backspace" => KeyCode::Backspace,
            s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
            _ => return None,
        };
        Some(Key { code, ctrl, shift })
    }

    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("C-");
        }
        if self.shift {
            s.push_str("S-");
        }
        s.push_str(&match self.code {
            KeyCode::Enter => "Enter".into(),
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::PageUp => "PageUp".into(),
            KeyCode::PageDown => "PageDown".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Char(c) => c.to_string(),
            other => format!("{other:?}"),
        });
        s
    }
}

fn key(code: KeyCode) -> Key {
    Key {
        code,
        ctrl: false,
        shift: false,
    }
}

/// A menu entry: the next key, what it does, its label.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub key: Key,
    pub action: Action,
    pub label: String,
    /// `Some(reason)` when shown but not runnable here (a task whose agent is missing).
    pub disabled: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Keymap {
    /// Full chords that fire anywhere.
    global: BTreeMap<Vec<Key>, Action>,
    /// Leader-menu entries: (scope, key after the leader) -> action.
    leader_menu: Vec<(Scope, Key, Action)>,
    /// Tasks menu: key -> (task name, label, disabled reason).
    tasks: Vec<(Key, String, String, Option<String>)>,
    leader: Key,
    tasks_key: Key,
    /// Built-in aliases that are always on (arrows, Esc) and never shown in menus.
    extra: Vec<(Vec<Key>, Action)>,
    pub overrides_errors: Vec<String>,
}

impl Keymap {
    /// The effective keymap for a project scope (`None` = all projects).
    pub fn from_config(cfg: &Config, project: Option<&Project>) -> Self {
        let keys = cfg.keys_for(project);
        let mut errors = vec![];
        let get = |name: &str| keys.get(name).map(String::as_str);
        let leader = get("leader")
            .and_then(Key::parse)
            .unwrap_or_else(|| key(KeyCode::Char(' ')));
        let tasks_key = get("tasks")
            .and_then(Key::parse)
            .unwrap_or_else(|| key(KeyCode::Char('t')));

        let mut global = BTreeMap::new();
        let mut leader_menu: Vec<(Scope, Key, Action)> = vec![];
        for a in Action::ALL {
            let (name, _, scope) = a.spec();
            let Some(v) = get(name) else {
                errors.push(format!(
                    "[keys] {name}: not bound (missing from the config)"
                ));
                continue;
            };
            let Some(chord) = parse_chord(v) else {
                errors.push(format!("[keys] {name} = {v:?}: cannot parse"));
                continue;
            };
            // A single plain key on a leader-scoped action goes under the leader; anything
            // else (a chord, a modified key, a global action) is a top-level binding.
            let single_plain = chord.len() == 1 && !chord[0].ctrl && !chord[0].shift;
            if scope != Scope::Global && single_plain {
                let k = chord[0];
                // Same key, same rows = a real clash — except agent vs worktree on an agent
                // row, where the agent action wins (resolution order below).
                if let Some((_, _, other)) = leader_menu.iter().find(|(s, kk, _)| {
                    *kk == k
                        && scopes_overlap(*s, scope)
                        && !matches!(
                            (*s, scope),
                            (Scope::Agent, Scope::Worktree) | (Scope::Worktree, Scope::Agent)
                        )
                }) {
                    errors.push(format!(
                        "[keys] {name} and {} both use {} after the leader for the same rows",
                        other.name(),
                        k.display()
                    ));
                    continue;
                }
                leader_menu.push((scope, k, a.clone()));
            } else {
                if let Some(other) = global.get(&chord) {
                    errors.push(format!(
                        "[keys] {name} and {} both use {v:?}",
                        Action::name(other)
                    ));
                    continue;
                }
                global.insert(chord, a.clone());
            }
        }

        // Tasks are listed from their definitions (key, label); one that cannot start on this
        // config (no agent yet, unknown agent) stays in the menu greyed with the reason.
        let mut tasks: Vec<(Key, String, String, Option<String>)> = vec![];
        for (name, r) in cfg.tasks_for(project) {
            let Some((key_s, label, enabled)) = cfg.task_key_label(&name, project) else {
                continue;
            };
            if !enabled {
                continue;
            }
            let Some(k) = key_s.as_deref().and_then(Key::parse) else {
                continue;
            };
            if let Some((_, other, _, _)) = tasks.iter().find(|(kk, _, _, _)| *kk == k) {
                errors.push(format!(
                    "[tasks] `{other}` and `{name}` both use key {}",
                    k.display()
                ));
                continue;
            }
            let disabled = r.err().map(|e| {
                let m = format!("{e:#}");
                m.lines()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(':')
                    .to_string()
            });
            tasks.push((k, name, label, disabled));
        }

        let extra = vec![
            (vec![key(KeyCode::Up)], Action::Up),
            (vec![key(KeyCode::Down)], Action::Down),
            (vec![key(KeyCode::Left)], Action::Collapse),
            (vec![key(KeyCode::Right)], Action::Expand),
            (vec![key(KeyCode::PageUp)], Action::PageUp),
            (vec![key(KeyCode::PageDown)], Action::PageDown),
            (
                vec![Key {
                    code: KeyCode::Char('c'),
                    ctrl: true,
                    shift: false,
                }],
                Action::Quit,
            ),
        ];
        Self {
            global,
            leader_menu,
            tasks,
            leader,
            tasks_key,
            extra,
            overrides_errors: errors,
        }
    }

    pub fn leader(&self) -> Key {
        self.leader
    }

    /// Resolve the keys pressed so far for the selected row kind. `Exact` = fire; `Prefix` =
    /// wait (show the menu); `None` = reset.
    pub fn resolve(&self, pressed: &[Key], row: RowKind) -> Resolution {
        if let Some(a) = self.global.get(pressed) {
            return Resolution::Exact(a.clone());
        }
        if let Some((_, a)) = self.extra.iter().find(|(k, _)| k.as_slice() == pressed) {
            return Resolution::Exact(a.clone());
        }
        if pressed.first() == Some(&self.leader) {
            match pressed.len() {
                1 => {
                    let applicable: Vec<&(Scope, Key, Action)> = self
                        .leader_menu
                        .iter()
                        .filter(|(s, _, _)| s.applies(row))
                        .collect();
                    let mut menu: Vec<MenuItem> = applicable
                        .iter()
                        .filter(|(s, k, _)| {
                            // hide a worktree entry shadowed by an agent entry on the same key
                            !(*s == Scope::Worktree
                                && applicable
                                    .iter()
                                    .any(|(s2, k2, _)| *s2 == Scope::Agent && k2 == k))
                        })
                        .map(|(_, k, a)| MenuItem {
                            key: *k,
                            action: a.clone(),
                            label: a.label().to_string(),
                            disabled: None,
                        })
                        .collect();
                    if !self.tasks.is_empty() {
                        menu.push(MenuItem {
                            key: self.tasks_key,
                            action: Action::Task(String::new()),
                            label: "tasks …".into(),
                            disabled: None,
                        });
                    }
                    return if menu.is_empty() {
                        Resolution::None
                    } else {
                        Resolution::Prefix(menu)
                    };
                }
                2 if pressed[1] == self.tasks_key && !self.tasks.is_empty() => {
                    return Resolution::Prefix(
                        self.tasks
                            .iter()
                            .map(|(k, n, l, d)| MenuItem {
                                key: *k,
                                action: Action::Task(n.clone()),
                                label: l.clone(),
                                disabled: d.clone(),
                            })
                            .collect(),
                    );
                }
                2 => {
                    // Most specific scope wins: an agent row prefers agent actions over
                    // worktree ones that share the key.
                    let rank = |s: &Scope| match s {
                        Scope::Agent | Scope::Review | Scope::Tmux | Scope::Machine => 0,
                        Scope::Worktree => 1,
                        Scope::Any => 2,
                        Scope::Global => 3,
                    };
                    return match self
                        .leader_menu
                        .iter()
                        .filter(|(s, k, _)| *k == pressed[1] && s.applies(row))
                        .min_by_key(|(s, _, _)| rank(s))
                    {
                        Some((_, _, a)) => Resolution::Exact(a.clone()),
                        None => Resolution::None,
                    };
                }
                3 if pressed[1] == self.tasks_key => {
                    return match self.tasks.iter().find(|(k, _, _, _)| *k == pressed[2]) {
                        Some((_, n, _, _)) => Resolution::Exact(Action::Task(n.clone())),
                        None => Resolution::None,
                    };
                }
                _ => return Resolution::None,
            }
        }
        let menu: Vec<MenuItem> = self
            .global
            .iter()
            .filter(|(k, _)| k.len() == pressed.len() + 1 && k.starts_with(pressed))
            .map(|(k, a)| MenuItem {
                key: k[pressed.len()],
                action: a.clone(),
                label: a.label().to_string(),
                disabled: None,
            })
            .collect();
        if menu.is_empty() {
            Resolution::None
        } else {
            Resolution::Prefix(menu)
        }
    }

    /// The chord that fires `action` (leader-menu entries as `Space x`; tasks as `Space t x`).
    pub fn chord_for(&self, action: &Action) -> Option<String> {
        if let Action::Task(n) = action {
            return self
                .tasks
                .iter()
                .find(|(_, t, _, _)| t == n)
                .map(|(k, _, _, _)| {
                    format!(
                        "{} {} {}",
                        self.leader.display(),
                        self.tasks_key.display(),
                        k.display()
                    )
                });
        }
        if let Some((k, _)) = self.global.iter().find(|(_, a)| *a == action) {
            return Some(k.iter().map(Key::display).collect::<Vec<_>>().join(" "));
        }
        self.leader_menu
            .iter()
            .find(|(_, _, a)| a == action)
            .map(|(_, k, _)| format!("{} {}", self.leader.display(), k.display()))
    }

    /// `(config name, chord)` for every action and task, for `fleet keys` and the help popup.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Action::ALL
            .iter()
            .map(|a| {
                (
                    a.name(),
                    self.chord_for(a).unwrap_or_else(|| "(unbound)".into()),
                )
            })
            .collect();
        for (_, n, _, _) in &self.tasks {
            let a = Action::Task(n.clone());
            v.push((a.name(), self.chord_for(&a).unwrap_or_default()));
        }
        v
    }

    /// Top-level prefixes and their labels, for the footer.
    pub fn prefixes(&self) -> Vec<(Key, &'static str)> {
        let mut v: Vec<(Key, &'static str)> = vec![(self.leader, "menu")];
        for (k, a) in &self.global {
            if k.len() == 2 && !v.iter().any(|(x, _)| *x == k[0]) {
                let label = match a.spec().0.split('.').next().unwrap_or("") {
                    "goto" | "top" => "goto",
                    "fold" | "unfold" => "fold",
                    "project" => "project",
                    _ => "more",
                };
                v.push((k[0], label));
            }
        }
        v
    }
}

fn scopes_overlap(a: Scope, b: Scope) -> bool {
    use Scope::*;
    a == b
        || matches!((a, b), (Any, _) | (_, Any))
        || matches!((a, b), (Agent, Worktree) | (Worktree, Agent))
}

pub enum Resolution {
    Exact(Action),
    Prefix(Vec<MenuItem>),
    None,
}

fn parse_chord(s: &str) -> Option<Vec<Key>> {
    let keys: Option<Vec<Key>> = s.split_whitespace().map(Key::parse).collect();
    keys.filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Key {
        Key::parse(s).unwrap()
    }

    fn with_agent(extra: &str) -> Config {
        Config::from_lua(
            &format!(r#"return {{ agents = {{ opencode = {{ harness = "opencode" }} }}, defaults = {{ agent = "opencode" }}, {extra} }}"#),
            "t",
        )
        .unwrap()
    }

    #[test]
    fn shipped_layout_resolves() {
        let cfg = with_agent("");
        let km = Keymap::from_config(&cfg, None);
        assert!(km.overrides_errors.is_empty(), "{:?}", km.overrides_errors);
        // global chords
        match km.resolve(&[k("g")], RowKind::Other) {
            Resolution::Prefix(menu) => {
                assert!(menu.iter().any(|m| m.action == Action::GotoAgents))
            }
            _ => panic!("g should be a prefix"),
        }
        assert!(matches!(
            km.resolve(&[k("g"), k("g")], RowKind::Other),
            Resolution::Exact(Action::Top)
        ));
        assert!(matches!(
            km.resolve(&[k("y")], RowKind::Agent),
            Resolution::Exact(Action::Yank)
        ));
        assert!(matches!(
            km.resolve(&[k("["), k("p")], RowKind::Other),
            Resolution::Exact(Action::ProjectPrev)
        ));
        // leader menu is row-aware: `s` stops an agent on an agent row, opens a shell on a worktree row
        assert!(matches!(
            km.resolve(&[k("Space"), k("s")], RowKind::Agent),
            Resolution::Exact(Action::AgentStop)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("S")], RowKind::Worktree),
            Resolution::Exact(Action::WorktreeShell)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("S")], RowKind::Agent),
            Resolution::Exact(Action::WorktreeShell)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("u")], RowKind::Agent),
            Resolution::Exact(Action::WorktreePush)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("x")], RowKind::Agent),
            Resolution::Exact(Action::WorktreeClose)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("x")], RowKind::Tmux),
            Resolution::Exact(Action::TmuxKill)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("r")], RowKind::Review),
            Resolution::Exact(Action::ReviewStart)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("r")], RowKind::Agent),
            Resolution::None
        ));
        match km.resolve(&[k("Space")], RowKind::Review) {
            Resolution::Prefix(menu) => {
                assert!(menu.iter().any(|m| m.action == Action::ReviewOpen));
                assert!(!menu.iter().any(|m| m.action == Action::AgentAttach));
                assert!(menu.iter().any(|m| m.label == "tasks …"));
            }
            _ => panic!("Space should open the leader menu"),
        }
        // tasks from the shipped config
        match km.resolve(&[k("Space"), k("t")], RowKind::Other) {
            Resolution::Prefix(menu) => {
                let names: Vec<_> = menu.iter().map(|m| m.action.clone()).collect();
                assert!(names.contains(&Action::Task("start".into())));
                assert!(names.contains(&Action::Task("review".into())));
                assert!(names.contains(&Action::Task("explain".into())));
            }
            _ => panic!("Space t should open the tasks menu"),
        }
        assert!(
            matches!(km.resolve(&[k("Space"), k("t"), k("e")], RowKind::Other), Resolution::Exact(Action::Task(ref n)) if n == "explain")
        );
        assert_eq!(
            km.chord_for(&Action::Task("start".into())).unwrap(),
            "Space t s"
        );
        assert_eq!(km.chord_for(&Action::AgentStop).unwrap(), "Space s");
    }

    #[test]
    fn overrides_and_project_tasks() {
        let cfg = with_agent(
            r#"keys = { ["agent.stop"] = "K", ["tab.reviews"] = "C-r" },
                tasks = { bugfix = { key = "b" } },
                projects = { { name = "p", repo = "/r", tasks = { release = { key = "R" } }, keys = { leader = "," } } }"#,
        );
        let km = Keymap::from_config(&cfg, None);
        assert!(km.overrides_errors.is_empty(), "{:?}", km.overrides_errors);
        // an uppercase single key is still "plain" and stays under the leader
        assert!(matches!(
            km.resolve(&[k("Space"), k("K")], RowKind::Agent),
            Resolution::Exact(Action::AgentStop)
        ));
        // agent `s` shadows nothing now, so Space s on an agent row is None
        assert!(matches!(
            km.resolve(&[k("Space"), k("s")], RowKind::Agent),
            Resolution::None
        ));
        assert!(matches!(
            km.resolve(&[k("C-r")], RowKind::Other),
            Resolution::Exact(Action::Tab4)
        ));
        assert!(
            matches!(km.resolve(&[k("Space"), k("t"), k("b")], RowKind::Other), Resolution::Exact(Action::Task(ref n)) if n == "bugfix")
        );
        assert!(matches!(
            km.resolve(&[k("Space"), k("t"), k("R")], RowKind::Other),
            Resolution::None
        ));
        let p = cfg.project("p").unwrap();
        let kp = Keymap::from_config(&cfg, Some(p));
        assert_eq!(kp.leader(), k(","));
        assert!(
            matches!(kp.resolve(&[k(","), k("t"), k("R")], RowKind::Other), Resolution::Exact(Action::Task(ref n)) if n == "release")
        );
    }

    #[test]
    fn clashes_are_reported() {
        let cfg = Config::from_lua(
            r#"return { agents = { opencode = { harness = "opencode" } }, defaults = { agent = "opencode" },
                        keys = { ["agent.attach"] = "s" }, tasks = { b1 = { key = "b" }, b2 = { key = "b" } } }"#,
            "t",
        );
        // two tasks on one key is fatal at config load
        assert!(cfg.is_err());
        let cfg = with_agent(r#"keys = { ["agent.attach"] = "s" }"#);
        let km = Keymap::from_config(&cfg, None);
        assert!(
            km.overrides_errors
                .iter()
                .any(|e| e.contains("agent.stop") && e.contains("agent.attach")),
            "{:?}",
            km.overrides_errors
        );
        // agent vs worktree on one key is allowed (agent wins on agent rows)
        let cfg =
            Config::from_lua(r#"return { keys = { ["worktree.shell"] = "s" } }"#, "t").unwrap();
        let km = Keymap::from_config(&cfg, None);
        assert!(km.overrides_errors.is_empty(), "{:?}", km.overrides_errors);
        assert!(matches!(
            km.resolve(&[k("Space"), k("s")], RowKind::Agent),
            Resolution::Exact(Action::AgentStop)
        ));
        assert!(matches!(
            km.resolve(&[k("Space"), k("s")], RowKind::Worktree),
            Resolution::Exact(Action::WorktreeShell)
        ));
    }
}
