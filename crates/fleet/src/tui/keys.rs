//! Actions, default bindings, user overrides, and which-key menus generated from them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fleet_core::Config;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Help,
    Refresh,
    RefreshReviews,
    Filter,
    Command,
    Detail,
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
    Copy,
    Collapse,
    Expand,
    Sort,
    // goto
    GotoAgents,
    GotoTmux,
    GotoWorktree,
    GotoReview,
    // agent
    AgentAttach,
    AgentStart,
    AgentSend,
    AgentRename,
    AgentStop,
    AgentOpenPlan,
    AgentCopyPlan,
    // worktree
    WorktreeShell,
    WorktreePush,
    WorktreePull,
    WorktreeRemove,
    WorktreeGc,
    WorktreeColumns,
    WorktreeOpenReview,
    // review
    ReviewOpen,
    ReviewCopy,
    ReviewStart,
    ReviewGotoWorktrees,
    ReviewGotoAgents,
    ReviewFilter,
    ProjectNext,
    ProjectPrev,
    FoldAll,
    UnfoldAll,
    // tmux
    TmuxKill,
    TmuxRename,
}

impl Action {
    pub const ALL: &'static [Action] = &[
        Action::Quit,
        Action::Help,
        Action::Refresh,
        Action::RefreshReviews,
        Action::Filter,
        Action::Command,
        Action::Detail,
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
        Action::Copy,
        Action::Collapse,
        Action::Expand,
        Action::Sort,
        Action::GotoAgents,
        Action::GotoTmux,
        Action::GotoWorktree,
        Action::GotoReview,
        Action::AgentAttach,
        Action::AgentStart,
        Action::AgentSend,
        Action::AgentRename,
        Action::AgentStop,
        Action::AgentOpenPlan,
        Action::AgentCopyPlan,
        Action::WorktreeShell,
        Action::WorktreePush,
        Action::WorktreePull,
        Action::WorktreeRemove,
        Action::WorktreeGc,
        Action::WorktreeColumns,
        Action::WorktreeOpenReview,
        Action::ReviewOpen,
        Action::ReviewCopy,
        Action::ReviewStart,
        Action::ReviewGotoWorktrees,
        Action::ReviewGotoAgents,
        Action::ReviewFilter,
        Action::ProjectNext,
        Action::ProjectPrev,
        Action::FoldAll,
        Action::UnfoldAll,
        Action::TmuxKill,
        Action::TmuxRename,
    ];

    /// Config key and default chord.
    pub fn spec(self) -> (&'static str, &'static str, &'static str) {
        // (config name, default keys, label)
        match self {
            Action::Quit => ("quit", "q", "quit"),
            Action::Help => ("help", "?", "help / cheat sheet"),
            Action::Refresh => ("refresh", "r", "refresh now"),
            Action::RefreshReviews => ("refresh.reviews", "R", "refresh reviews now"),
            Action::Filter => ("filter", "/", "filter rows"),
            Action::Command => ("command", ":", "command line"),
            Action::Detail => ("detail", "Space", "toggle detail panel"),
            Action::Up => ("up", "k", "up"),
            Action::Down => ("down", "j", "down"),
            Action::Top => ("top", "g g", "top"),
            Action::Bottom => ("bottom", "G", "bottom"),
            Action::PageUp => ("page.up", "C-u", "page up"),
            Action::PageDown => ("page.down", "C-d", "page down"),
            Action::Tab1 => ("tab.agents", "1", "Agents tab"),
            Action::Tab2 => ("tab.tmux", "2", "Tmux tab"),
            Action::Tab3 => ("tab.worktrees", "3", "Worktrees tab"),
            Action::Tab4 => ("tab.reviews", "4", "Reviews tab"),
            Action::NextTab => ("tab.next", "Tab", "next tab"),
            Action::PrevTab => ("tab.prev", "S-Tab", "previous tab"),
            Action::Primary => ("primary", "Enter", "attach / shell / review"),
            Action::Open => ("open", "o", "open plan or review in browser"),
            Action::Copy => ("copy", "c", "copy that URL"),
            Action::Collapse => ("collapse", "h", "collapse"),
            Action::Expand => ("expand", "l", "expand"),
            Action::Sort => ("sort", "S", "cycle sort"),
            Action::GotoAgents => ("goto.agents", "g a", "this row in Agents"),
            Action::GotoTmux => ("goto.tmux", "g t", "this row in Tmux"),
            Action::GotoWorktree => ("goto.worktree", "g w", "this row in Worktrees"),
            Action::GotoReview => ("goto.review", "g v", "this row in Reviews"),
            Action::AgentAttach => ("agent.attach", "a a", "attach (tmux)"),
            Action::AgentStart => ("agent.start", "a s", "start new agent"),
            Action::AgentSend => ("agent.send", "a m", "send message"),
            Action::AgentRename => ("agent.rename", "a n", "rename task"),
            Action::AgentStop => ("agent.stop", "a x", "stop agent (confirm)"),
            Action::AgentOpenPlan => ("agent.open_plan", "a o", "open plan URL"),
            Action::AgentCopyPlan => ("agent.copy_plan", "a c", "copy plan URL"),
            Action::WorktreeShell => ("worktree.shell", "w s", "shell here (tmux window)"),
            Action::WorktreePush => ("worktree.push", "w p", "push -u"),
            Action::WorktreePull => ("worktree.pull", "w P", "pull --ff-only"),
            Action::WorktreeRemove => ("worktree.remove", "w D", "remove worktree (confirm)"),
            Action::WorktreeGc => ("worktree.gc", "w g", "gc (dry run)"),
            Action::WorktreeColumns => ("worktree.columns", "w c", "toggle name/path columns"),
            Action::WorktreeOpenReview => ("worktree.open_review", "w o", "open its review"),
            Action::ReviewOpen => ("review.open", "v o", "open in browser"),
            Action::ReviewCopy => ("review.copy", "v c", "copy URL"),
            Action::ReviewStart => ("review.start", "v r", "review / re-review (agent)"),
            Action::ReviewGotoWorktrees => ("review.worktrees", "v w", "its worktrees"),
            Action::ReviewGotoAgents => ("review.agents", "v a", "its agents"),
            Action::ReviewFilter => (
                "review.filter",
                "v f",
                "filter: all/mine/others/needs-me/failed",
            ),
            Action::ProjectNext => ("project.next", "p", "next project (… → all)"),
            Action::ProjectPrev => ("project.prev", "P", "previous project"),
            Action::FoldAll => ("fold.all", "z c", "fold all"),
            Action::UnfoldAll => ("unfold.all", "z o", "unfold all"),
            Action::TmuxKill => ("tmux.kill", "K", "kill session (confirm)"),
            Action::TmuxRename => ("tmux.rename", "n", "rename session"),
        }
    }

    pub fn label(self) -> &'static str {
        self.spec().2
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

#[derive(Debug, Clone)]
pub struct Keymap {
    /// chord -> action
    bindings: BTreeMap<Vec<Key>, Action>,
    /// Built-in aliases that are always on (arrows, Esc) and never shown in menus.
    extra: Vec<(Vec<Key>, Action)>,
    pub overrides_errors: Vec<String>,
}

impl Keymap {
    pub fn from_config(cfg: &Config) -> Self {
        let mut bindings = BTreeMap::new();
        let mut errors = vec![];
        for a in Action::ALL {
            let (name, default, _) = a.spec();
            let chord_s = cfg.keys.get(name).map(String::as_str).unwrap_or(default);
            match parse_chord(chord_s) {
                Some(c) => {
                    bindings.insert(c, *a);
                }
                None => errors.push(format!("[keys] {name} = {chord_s:?}: cannot parse")),
            }
        }
        let extra = vec![
            (
                vec![Key {
                    code: KeyCode::Up,
                    ctrl: false,
                    shift: false,
                }],
                Action::Up,
            ),
            (
                vec![Key {
                    code: KeyCode::Down,
                    ctrl: false,
                    shift: false,
                }],
                Action::Down,
            ),
            (
                vec![Key {
                    code: KeyCode::Left,
                    ctrl: false,
                    shift: false,
                }],
                Action::Collapse,
            ),
            (
                vec![Key {
                    code: KeyCode::Right,
                    ctrl: false,
                    shift: false,
                }],
                Action::Expand,
            ),
            (
                vec![Key {
                    code: KeyCode::PageUp,
                    ctrl: false,
                    shift: false,
                }],
                Action::PageUp,
            ),
            (
                vec![Key {
                    code: KeyCode::PageDown,
                    ctrl: false,
                    shift: false,
                }],
                Action::PageDown,
            ),
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
            bindings,
            extra,
            overrides_errors: errors,
        }
    }

    /// Resolve the keys pressed so far. `Exact` = fire; `Prefix` = wait (show menu); `None` = reset.
    pub fn resolve(&self, pressed: &[Key]) -> Resolution {
        if let Some(a) = self.bindings.get(pressed) {
            return Resolution::Exact(*a);
        }
        if let Some((_, a)) = self.extra.iter().find(|(k, _)| k.as_slice() == pressed) {
            return Resolution::Exact(*a);
        }
        let menu: Vec<(Key, Action)> = self
            .bindings
            .iter()
            .filter(|(k, _)| k.len() == pressed.len() + 1 && k.starts_with(pressed))
            .map(|(k, a)| (k[pressed.len()], *a))
            .collect();
        if menu.is_empty() {
            Resolution::None
        } else {
            Resolution::Prefix(menu)
        }
    }

    pub fn chord_for(&self, action: Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, a)| **a == action)
            .map(|(k, _)| k.iter().map(Key::display).collect::<Vec<_>>().join(" "))
    }

    /// `(config name, chord)` for every action, for `fleet keys` and the help popup.
    pub fn list(&self) -> Vec<(String, String)> {
        Action::ALL
            .iter()
            .map(|a| {
                (
                    a.spec().0.to_string(),
                    self.chord_for(*a).unwrap_or_else(|| "(unbound)".into()),
                )
            })
            .collect()
    }

    /// Every top-level prefix and its label, for the footer.
    pub fn prefixes(&self) -> Vec<(Key, &'static str)> {
        let mut v: Vec<(Key, &'static str)> = vec![];
        for (k, a) in &self.bindings {
            if k.len() == 2 {
                let label = match a.spec().0.split('.').next().unwrap_or("") {
                    "goto" | "top" => "goto",
                    "agent" => "agent",
                    "worktree" => "worktree",
                    "review" => "review",
                    "fold" | "unfold" => "fold",
                    _ => "more",
                };
                if !v.iter().any(|(x, _)| *x == k[0]) {
                    v.push((k[0], label));
                }
            }
        }
        v
    }
}

pub enum Resolution {
    Exact(Action),
    Prefix(Vec<(Key, Action)>),
    None,
}

fn parse_chord(s: &str) -> Option<Vec<Key>> {
    let keys: Option<Vec<Key>> = s.split_whitespace().map(Key::parse).collect();
    keys.filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let mut cfg = Config::default();
        cfg.keys.insert("agent.start".into(), "C-n".into());
        let km = Keymap::from_config(&cfg);
        assert!(km.overrides_errors.is_empty());
        assert_eq!(km.chord_for(Action::AgentStart).unwrap(), "C-n");
        assert_eq!(km.chord_for(Action::GotoAgents).unwrap(), "g a");
        let g = Key::parse("g").unwrap();
        match km.resolve(&[g]) {
            Resolution::Prefix(menu) => {
                assert!(menu.iter().any(|(_, a)| *a == Action::GotoAgents))
            }
            _ => panic!("g should be a prefix"),
        }
        match km.resolve(&[g, Key::parse("g").unwrap()]) {
            Resolution::Exact(Action::Top) => {}
            _ => panic!("g g should be Top"),
        }
    }
}
