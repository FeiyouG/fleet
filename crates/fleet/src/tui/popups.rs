use super::app::App;
use super::keys::{Action, Key};
use super::util::{accent, dim};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fleet_core::cmd::gc::{self, GcPlan};
use fleet_core::Config;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use tui_textarea::TextArea;

/// What a popup asks the app to do on submit.
#[derive(Debug, Clone)]
pub enum PopupCmd {
    SetFilter(String),
    SetProject(Option<String>),
    StartReview(String),
    RunCommand(String),
    Start {
        name: Option<String>,
        prompt: String,
        repo: String,
        agent: Option<String>,
        mode: Option<String>,
        model: Option<String>,
    },
    Message {
        name: String,
        text: String,
        mode: Option<String>,
        model: Option<String>,
    },
    Rename {
        old: String,
        new: String,
    },
    Stop(String),
    KillTmux(String),
    TmuxRename {
        old: String,
        new: String,
    },
    RemoveWorktree {
        repo: String,
        path: std::path::PathBuf,
    },
    ApplyGc(GcPlan),
}

pub enum Popup {
    Help,
    Input {
        title: String,
        value: String,
        kind: InputKind,
    },
    Confirm {
        text: String,
        cmd: PopupCmd,
    },
    Start(StartForm),
    Message(MessageForm),
    Gc {
        plan: GcPlan,
        checked: Vec<bool>,
        selected: usize,
    },
    ProjectPick {
        options: Vec<Option<String>>,
        selected: usize,
    },
}

#[derive(Debug, Clone)]
pub enum InputKind {
    Filter,
    Command,
    Rename(String),
    TmuxRename(String),
}

pub struct StartForm {
    pub name: TextArea<'static>,
    pub prompt: TextArea<'static>,
    pub repos: Vec<String>,
    pub repo_idx: usize,
    /// Enabled adapters (opencode, claude, codex).
    pub agents: Vec<String>,
    pub agent_idx: usize,
    /// Cycles through the modes the selected adapter offers in the repo.
    pub mode: String,
    /// `None` = default (`agents.<name>.model`, else the CLI's own).
    pub model: Option<String>,
    pub picker: Option<Picker>,
    /// 0 name, 1 repo, 2 agent, 3 mode, 4 model, 5 prompt
    pub focus: usize,
}

const START_FIELDS: usize = 6;
const FOCUS_PROMPT: usize = 5;

impl StartForm {
    pub fn repo_dir(&self, cfg: &Config) -> Option<std::path::PathBuf> {
        let name = self.repos.get(self.repo_idx)?;
        cfg.project(name).map(|p| p.repo.clone())
    }
    pub fn agent(&self) -> &str {
        self.agents
            .get(self.agent_idx)
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// A message to a running agent, optionally switching mode and/or model for it.
pub struct MessageForm {
    pub name: String,
    /// The agent's adapter and directory: where its choices come from.
    pub adapter: String,
    pub cwd: std::path::PathBuf,
    /// What the agent uses now (shown, and the meaning of "unchanged").
    pub current_mode: Option<String>,
    pub current_model: Option<String>,
    pub text: String,
    /// `None` = unchanged.
    pub mode: Option<String>,
    /// `None` = unchanged.
    pub model: Option<String>,
    pub picker: Option<Picker>,
}

/// The filterable model list opened from a form.
#[derive(Default)]
pub struct Picker {
    pub filter: String,
    pub selected: usize,
}

enum Picked {
    Keep,
    Cancel,
    Pick(Option<String>),
}

/// Entries of the picker: `None` first (default / unchanged), then models matching every
/// space-separated word of the filter (case-insensitive). An adapter that lists no models
/// (claude, codex) offers the typed text itself.
fn picker_items(models: &[String], filter: &str) -> Vec<Option<String>> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    let mut out = vec![];
    if words.is_empty() {
        out.push(None);
    }
    if models.is_empty() {
        if !filter.trim().is_empty() {
            out.push(Some(filter.trim().to_string()));
        }
        return out;
    }
    out.extend(
        models
            .iter()
            .filter(|m| {
                let l = m.to_lowercase();
                words.iter().all(|w| l.contains(w))
            })
            .map(|m| Some(m.clone())),
    );
    out
}

fn picker_key(p: &mut Picker, ev: &KeyEvent, models: &[String]) -> Picked {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let items = picker_items(models, &p.filter);
    let last = items.len().saturating_sub(1);
    match ev.code {
        KeyCode::Esc => return Picked::Cancel,
        KeyCode::Enter => {
            return match items.get(p.selected) {
                Some(i) => Picked::Pick(i.clone()),
                None => Picked::Keep,
            }
        }
        KeyCode::Up => p.selected = p.selected.saturating_sub(1),
        KeyCode::Char('p') | KeyCode::Char('k') if ctrl => {
            p.selected = p.selected.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Tab => p.selected = (p.selected + 1).min(last),
        KeyCode::Char('n') | KeyCode::Char('j') if ctrl => p.selected = (p.selected + 1).min(last),
        KeyCode::PageDown => p.selected = (p.selected + 10).min(last),
        KeyCode::PageUp => p.selected = p.selected.saturating_sub(10),
        KeyCode::Backspace => {
            p.filter.pop();
            p.selected = 0;
        }
        KeyCode::Char('u') if ctrl => {
            p.filter.clear();
            p.selected = 0;
        }
        KeyCode::Char(c) if !ctrl => {
            p.filter.push(c);
            p.selected = 0;
        }
        _ => {}
    }
    Picked::Keep
}

fn loaded<'a>(
    app: &'a App,
    adapter: &str,
    dir: Option<&std::path::Path>,
) -> Option<&'a fleet_core::agents::Choices> {
    dir.and_then(|d| app.choices_for(adapter, d))
        .and_then(|r| r.as_ref().ok())
}

/// The models `adapter` offers in `dir`, or empty while loading / on error / unlisted.
fn models_for(app: &App, adapter: &str, dir: Option<&std::path::Path>) -> Vec<String> {
    loaded(app, adapter, dir)
        .map(|c| c.models.clone())
        .unwrap_or_default()
}

/// Mode names `adapter` offers in `dir`; while loading (or on error) fleet's plan/build.
fn modes_for(app: &App, adapter: &str, dir: Option<&std::path::Path>) -> Vec<String> {
    match loaded(app, adapter, dir) {
        Some(c) if !c.agents.is_empty() => c.agent_names(),
        _ => vec!["plan".into(), "build".into()],
    }
}

/// `loading…` / the error / `None` when the choices are ready.
fn choices_note(app: &App, adapter: &str, dir: Option<&std::path::Path>) -> Option<String> {
    match dir.and_then(|d| app.choices_for(adapter, d)) {
        None if dir.is_some() => Some("loading modes and models…".into()),
        None => Some("no repo".into()),
        Some(Err(e)) => Some(format!("could not list models: {e}")),
        Some(Ok(_)) => None,
    }
}

/// What "default" means in the start form: the adapter's configured model, else the
/// CLI's own default for the mode.
fn default_model_label(
    app: &App,
    adapter: &str,
    dir: Option<&std::path::Path>,
    mode: &str,
) -> String {
    if let Some(m) = app.cfg.agents.get(adapter).model {
        return format!("default ({m})");
    }
    match loaded(app, adapter, dir).and_then(|c| c.default_for(mode)) {
        Some(m) => format!("default ({m})"),
        None => format!("default ({adapter} decides)"),
    }
}

fn cycle(list: &[String], cur: &str, back: bool) -> String {
    if list.is_empty() {
        return cur.to_string();
    }
    let n = list.len();
    let next = match list.iter().position(|x| x == cur) {
        Some(i) if back => (i + n - 1) % n,
        Some(i) => (i + 1) % n,
        None => 0,
    };
    list[next].clone()
}

pub enum Outcome {
    Keep(Popup),
    Close,
    Submit(PopupCmd),
}

impl Popup {
    pub fn help(_app: &App) -> Popup {
        Popup::Help
    }
    pub fn filter(current: String) -> Popup {
        Popup::Input {
            title: "filter".into(),
            value: current,
            kind: InputKind::Filter,
        }
    }
    pub fn command() -> Popup {
        Popup::Input {
            title: ": fleet".into(),
            value: String::new(),
            kind: InputKind::Command,
        }
    }
    pub fn message(name: String, agent: fleet_core::model::AgentRow) -> Popup {
        Popup::Message(MessageForm {
            name,
            adapter: agent.adapter,
            cwd: agent.cwd,
            current_mode: agent.mode,
            current_model: agent.model,
            text: String::new(),
            mode: None,
            model: None,
            picker: None,
        })
    }
    pub fn rename(old: String) -> Popup {
        Popup::Input {
            title: format!("rename {old} to"),
            value: old.clone(),
            kind: InputKind::Rename(old),
        }
    }
    pub fn tmux_rename(old: String) -> Popup {
        Popup::Input {
            title: format!("rename tmux session {old} to"),
            value: old.clone(),
            kind: InputKind::TmuxRename(old),
        }
    }
    pub fn confirm(text: String, cmd: PopupCmd) -> Popup {
        Popup::Confirm { text, cmd }
    }
    pub fn gc(plan: GcPlan) -> Popup {
        let n = plan.actions.len();
        Popup::Gc {
            plan,
            checked: vec![true; n],
            selected: 0,
        }
    }
    pub fn project(cfg: &Config, current: Option<String>) -> Popup {
        let mut options: Vec<Option<String>> =
            cfg.projects.iter().map(|p| Some(p.name.clone())).collect();
        options.push(None);
        let selected = options.iter().position(|o| *o == current).unwrap_or(0);
        Popup::ProjectPick { options, selected }
    }
    pub fn start(cfg: &Config) -> Popup {
        let mut name = TextArea::default();
        name.set_placeholder_text("optional · leave empty for wip-xxxx (the agent renames itself)");
        name.set_cursor_line_style(Style::default());
        let mut prompt = TextArea::default();
        prompt
            .set_placeholder_text("first prompt for the agent (C-e opens $EDITOR) · C-s to start");
        prompt.set_cursor_line_style(Style::default());
        let repos: Vec<String> = cfg.projects.iter().map(|r| r.name.clone()).collect();
        let cwd_repo = std::env::current_dir()
            .ok()
            .and_then(|c| cfg.project_for_path(&c).map(|r| r.name.clone()));
        let repo_idx = cwd_repo
            .and_then(|n| repos.iter().position(|r| *r == n))
            .unwrap_or(0);
        let agents: Vec<String> = cfg.agents.enabled().into_iter().map(String::from).collect();
        let agent_idx = agents
            .iter()
            .position(|a| *a == cfg.defaults.agent)
            .unwrap_or(0);
        Popup::Start(StartForm {
            name,
            prompt,
            repos,
            repo_idx,
            agents,
            agent_idx,
            mode: cfg.defaults.mode.clone(),
            model: None,
            picker: None,
            focus: FOCUS_PROMPT,
        })
    }
}

pub fn handle(p: Popup, ev: &KeyEvent, app: &mut App) -> Outcome {
    let esc = ev.code == KeyCode::Esc;
    match p {
        Popup::Help => Outcome::Close,
        Popup::Input {
            title,
            mut value,
            kind,
        } => {
            match ev.code {
                KeyCode::Esc => Outcome::Close,
                KeyCode::Enter => {
                    let v = value.trim().to_string();
                    match kind {
                        InputKind::Filter => Outcome::Submit(PopupCmd::SetFilter(v)),
                        InputKind::Command => {
                            if v.is_empty() {
                                Outcome::Close
                            } else {
                                Outcome::Submit(PopupCmd::RunCommand(v))
                            }
                        }
                        InputKind::Rename(old) => {
                            if v.is_empty() || v == old {
                                Outcome::Close
                            } else {
                                Outcome::Submit(PopupCmd::Rename { old, new: v })
                            }
                        }
                        InputKind::TmuxRename(old) => {
                            if v.is_empty() || v == old {
                                Outcome::Close
                            } else {
                                Outcome::Submit(PopupCmd::TmuxRename { old, new: v })
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                    // live filter
                    if matches!(kind, InputKind::Filter) {
                        app.tabs.state_mut(app.tab).filter = value.clone();
                    }
                    Outcome::Keep(Popup::Input { title, value, kind })
                }
                KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear();
                    if matches!(kind, InputKind::Filter) {
                        app.tabs.state_mut(app.tab).filter.clear();
                    }
                    Outcome::Keep(Popup::Input { title, value, kind })
                }
                KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.push(c);
                    if matches!(kind, InputKind::Filter) {
                        app.tabs.state_mut(app.tab).filter = value.clone();
                        app.tabs.state_mut(app.tab).selected = 0;
                    }
                    Outcome::Keep(Popup::Input { title, value, kind })
                }
                _ => Outcome::Keep(Popup::Input { title, value, kind }),
            }
        }
        Popup::Confirm { text, cmd } => match ev.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Outcome::Submit(cmd),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                Outcome::Close
            }
            _ => Outcome::Keep(Popup::Confirm { text, cmd }),
        },
        Popup::Gc {
            plan,
            mut checked,
            mut selected,
        } => match ev.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                selected = (selected + 1).min(plan.actions.len().saturating_sub(1));
                Outcome::Keep(Popup::Gc {
                    plan,
                    checked,
                    selected,
                })
            }
            KeyCode::Char('k') | KeyCode::Up => {
                selected = selected.saturating_sub(1);
                Outcome::Keep(Popup::Gc {
                    plan,
                    checked,
                    selected,
                })
            }
            KeyCode::Char(' ') => {
                if let Some(c) = checked.get_mut(selected) {
                    *c = !*c;
                }
                Outcome::Keep(Popup::Gc {
                    plan,
                    checked,
                    selected,
                })
            }
            KeyCode::Char('a') => {
                let all = checked.iter().all(|c| *c);
                checked.iter_mut().for_each(|c| *c = !all);
                Outcome::Keep(Popup::Gc {
                    plan,
                    checked,
                    selected,
                })
            }
            KeyCode::Enter => {
                let actions: Vec<_> = plan
                    .actions
                    .iter()
                    .zip(&checked)
                    .filter(|(_, c)| **c)
                    .map(|(a, _)| a.clone())
                    .collect();
                if actions.is_empty() {
                    Outcome::Close
                } else {
                    Outcome::Submit(PopupCmd::ApplyGc(GcPlan {
                        actions,
                        blocked: plan.blocked.clone(),
                    }))
                }
            }
            _ => Outcome::Keep(Popup::Gc {
                plan,
                checked,
                selected,
            }),
        },
        Popup::ProjectPick {
            options,
            mut selected,
        } => match ev.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                selected = (selected + 1).min(options.len() - 1);
                Outcome::Keep(Popup::ProjectPick { options, selected })
            }
            KeyCode::Char('k') | KeyCode::Up => {
                selected = selected.saturating_sub(1);
                Outcome::Keep(Popup::ProjectPick { options, selected })
            }
            KeyCode::Enter => Outcome::Submit(PopupCmd::SetProject(options[selected].clone())),
            _ => Outcome::Keep(Popup::ProjectPick { options, selected }),
        },
        Popup::Start(mut form) => {
            let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
            let dir = form.repo_dir(&app.cfg);
            let adapter = form.agent().to_string();
            if let Some(mut p) = form.picker.take() {
                match picker_key(&mut p, ev, &models_for(app, &adapter, dir.as_deref())) {
                    Picked::Keep => form.picker = Some(p),
                    Picked::Cancel => {}
                    Picked::Pick(m) => form.model = m,
                }
                return Outcome::Keep(Popup::Start(form));
            }
            if esc {
                return Outcome::Close;
            }
            if ctrl && ev.code == KeyCode::Char('s') {
                let prompt = form.prompt.lines().join("\n").trim().to_string();
                if prompt.is_empty() {
                    return Outcome::Keep(Popup::Start(form));
                }
                let name = form.name.lines().join("").trim().to_string();
                return Outcome::Submit(PopupCmd::Start {
                    name: if name.is_empty() { None } else { Some(name) },
                    prompt,
                    repo: form.repos.get(form.repo_idx).cloned().unwrap_or_default(),
                    agent: form.agents.get(form.agent_idx).cloned(),
                    mode: Some(form.mode.clone()),
                    model: form.model.clone(),
                });
            }
            if ctrl && ev.code == KeyCode::Char('e') {
                // Edit the prompt in $EDITOR: run it inline (blocking) through a suspend.
                let current = form.prompt.lines().join("\n");
                match edit_in_editor(&current) {
                    Ok(Some(text)) => {
                        form.prompt =
                            TextArea::from(text.lines().map(String::from).collect::<Vec<_>>());
                        form.prompt.set_cursor_line_style(Style::default());
                    }
                    Ok(None) => {}
                    Err(e) => app.set_status(format!("editor: {e:#}"), true),
                }
                return Outcome::Keep(Popup::Start(form));
            }
            if ev.code == KeyCode::Tab || (ev.code == KeyCode::BackTab) {
                let back =
                    ev.code == KeyCode::BackTab || ev.modifiers.contains(KeyModifiers::SHIFT);
                form.focus = if back {
                    (form.focus + START_FIELDS - 1) % START_FIELDS
                } else {
                    (form.focus + 1) % START_FIELDS
                };
                return Outcome::Keep(Popup::Start(form));
            }
            let prev = matches!(
                ev.code,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('k')
            );
            let next = matches!(
                ev.code,
                KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('j')
                    | KeyCode::Char(' ')
                    | KeyCode::Enter
            );
            let step = |i: usize, n: usize| {
                let n = n.max(1);
                if prev {
                    (i + n - 1) % n
                } else {
                    (i + 1) % n
                }
            };
            match form.focus {
                0 => {
                    if ev.code != KeyCode::Enter {
                        form.name.input(*ev);
                    } else {
                        form.focus = FOCUS_PROMPT;
                    }
                }
                // repo / agent: what the mode and model lists depend on
                1 | 2 if prev || next => {
                    if form.focus == 1 {
                        form.repo_idx = step(form.repo_idx, form.repos.len());
                    } else {
                        form.agent_idx = step(form.agent_idx, form.agents.len());
                        // a model name belongs to one adapter
                        form.model = None;
                    }
                    let dir = form.repo_dir(&app.cfg);
                    let adapter = form.agent().to_string();
                    if let Some(d) = dir.clone() {
                        app.want_choices(&adapter, d);
                    }
                    if !modes_for(app, &adapter, dir.as_deref()).contains(&form.mode) {
                        form.mode = "plan".into();
                    }
                }
                3 if prev || next => {
                    form.mode = cycle(&modes_for(app, &adapter, dir.as_deref()), &form.mode, prev)
                }
                4 => match ev.code {
                    KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => {
                        form.picker = Some(Picker::default())
                    }
                    KeyCode::Backspace | KeyCode::Delete => form.model = None,
                    _ => {}
                },
                FOCUS_PROMPT => {
                    form.prompt.input(*ev);
                }
                _ => {}
            }
            Outcome::Keep(Popup::Start(form))
        }
        Popup::Message(mut form) => {
            let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
            let dir = Some(form.cwd.as_path());
            if let Some(mut p) = form.picker.take() {
                match picker_key(&mut p, ev, &models_for(app, &form.adapter, dir)) {
                    Picked::Keep => form.picker = Some(p),
                    Picked::Cancel => {}
                    Picked::Pick(m) => form.model = m,
                }
                return Outcome::Keep(Popup::Message(form));
            }
            match ev.code {
                KeyCode::Esc => return Outcome::Close,
                KeyCode::Enter => {
                    let text = form.text.trim().to_string();
                    if text.is_empty() {
                        return Outcome::Close;
                    }
                    return Outcome::Submit(PopupCmd::Message {
                        name: form.name,
                        text,
                        mode: form.mode,
                        model: form.model,
                    });
                }
                // Tab cycles the mode: unchanged -> each mode -> unchanged.
                KeyCode::Tab | KeyCode::BackTab => {
                    let mut opts = vec![String::new()];
                    opts.extend(modes_for(app, &form.adapter, dir));
                    let cur = form.mode.clone().unwrap_or_default();
                    let next = cycle(&opts, &cur, ev.code == KeyCode::BackTab);
                    form.mode = (!next.is_empty()).then_some(next);
                }
                KeyCode::Char('o') if ctrl => form.picker = Some(Picker::default()),
                KeyCode::Backspace => {
                    form.text.pop();
                }
                KeyCode::Char('u') if ctrl => form.text.clear(),
                KeyCode::Char(c) if !ctrl => form.text.push(c),
                _ => {}
            }
            Outcome::Keep(Popup::Message(form))
        }
    }
}

/// Suspend the TUI, run `$EDITOR` on a temp file, resume. Returns `None` when unchanged.
fn edit_in_editor(current: &str) -> anyhow::Result<Option<String>> {
    use crossterm::{execute, terminal};
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("fleet-prompt-{}.md", std::process::id()));
    std::fs::write(&path, current)?;
    terminal::disable_raw_mode()?;
    execute!(std::io::stdout(), terminal::LeaveAlternateScreen)?;
    let parts = shell_words::split(&editor)?;
    let st = std::process::Command::new(&parts[0])
        .args(&parts[1..])
        .arg(&path)
        .status();
    execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    st?;
    let text = std::fs::read_to_string(&path)?;
    std::fs::remove_file(&path).ok();
    Ok(if text == current { None } else { Some(text) })
}

// ───────────────────────── drawing ─────────────────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn frame_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(format!(" {title} "), accent()))
}

pub fn draw_whichkey(f: &mut Frame, area: Rect, pending: &[Key], menu: &[(Key, Action)]) {
    let prefix = pending
        .iter()
        .map(Key::display)
        .collect::<Vec<_>>()
        .join(" ");
    let mut items: Vec<Line> = menu
        .iter()
        .map(|(k, a)| {
            Line::from(vec![
                Span::styled(format!(" {:<6}", k.display()), accent()),
                Span::raw(a.label().to_string()),
            ])
        })
        .collect();
    items.sort_by_key(|l| l.to_string());
    let h = (items.len() as u16 + 2).min(area.height.saturating_sub(2));
    let w = 46;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w + 1),
        y: area.y + area.height.saturating_sub(h + 1),
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(items).block(frame_block(&format!("{prefix} …"))),
        rect,
    );
}

pub fn draw(f: &mut Frame, area: Rect, p: &Popup, app: &App) {
    match p {
        Popup::Help => {
            let mut lines = vec![Line::from(Span::styled(
                "keys (defaults; override in ~/.config/fleet/config.toml [keys])",
                dim(),
            ))];
            let mut group = "";
            for a in Action::ALL {
                let (name, _, label) = a.spec();
                let g = name.split('.').next().unwrap_or("");
                let g = match g {
                    "goto" | "top" | "bottom" | "fold" | "unfold" | "project" => "navigation",
                    "agent" => "agent",
                    "worktree" => "worktree",
                    "review" => "review",
                    "tmux" => "tmux",
                    _ => "general",
                };
                if g != group {
                    group = g;
                    lines.push(Line::from(Span::styled(format!("── {g} "), accent())));
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            " {:<8}",
                            app.keymap.chord_for(*a).unwrap_or_else(|| "—".into())
                        ),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(format!("{label:<36}")),
                    Span::styled(name.to_string(), dim()),
                ]));
            }
            lines.push(Line::from(Span::styled("Esc closes", dim())));
            let rect = centered(area, 80, lines.len() as u16 + 2);
            f.render_widget(Clear, rect);
            f.render_widget(Paragraph::new(lines).block(frame_block("help")), rect);
        }
        Popup::Input { title, value, .. } => {
            let rect = centered(area, 80, 3);
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(format!(" {value}")),
                    Span::styled("▏", accent()),
                ]))
                .block(frame_block(title)),
                rect,
            );
        }
        Popup::Confirm { text, .. } => {
            let rect = centered(area, 70, 5);
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(text.clone()),
                    Line::from(""),
                    Line::from(Span::styled("y / Enter = yes · n / Esc = no", dim())),
                ])
                .wrap(Wrap { trim: false })
                .block(frame_block("confirm")),
                rect,
            );
        }
        Popup::Gc {
            plan,
            checked,
            selected,
        } => {
            let mut items: Vec<ListItem> = plan
                .actions
                .iter()
                .zip(checked)
                .enumerate()
                .map(|(i, (a, c))| {
                    let mark = if *c { "[x]" } else { "[ ]" };
                    let style = if i == *selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::from(Span::styled(
                        format!(" {mark} {}", gc::describe(a)),
                        style,
                    )))
                })
                .collect();
            if plan.actions.is_empty() {
                items.push(ListItem::new(Span::styled(" nothing to clean", dim())));
            }
            for (n, r) in &plan.blocked {
                items.push(ListItem::new(Span::styled(
                    format!(" blocked: {n} ({r})"),
                    dim(),
                )));
            }
            items.push(ListItem::new(Span::styled(
                " Space toggle · a all · Enter apply checked · Esc cancel",
                dim(),
            )));
            let rect = centered(area, 100, items.len() as u16 + 2);
            f.render_widget(Clear, rect);
            f.render_widget(
                List::new(items).block(frame_block("worktree gc (dry run)")),
                rect,
            );
        }
        Popup::ProjectPick { options, selected } => {
            let items: Vec<ListItem> = options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let style = if i == *selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Span::styled(
                        format!(" {}", o.as_deref().unwrap_or("all")),
                        style,
                    ))
                })
                .collect();
            let rect = centered(area, 50, items.len() as u16 + 2);
            f.render_widget(Clear, rect);
            f.render_widget(List::new(items).block(frame_block("project")), rect);
        }
        Popup::Message(form) => {
            let rect = centered(area, 90, 6);
            f.render_widget(Clear, rect);
            let title = format!(
                "message to {}  ·  Enter send · Tab mode · C-o model · Esc cancel",
                form.name
            );
            let block = frame_block(&title);
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let dir = Some(form.cwd.as_path());
            let now = |v: &Option<String>| v.clone().unwrap_or_else(|| "?".into());
            let mode = match &form.mode {
                Some(a) => Span::styled(a.clone(), accent()),
                None => Span::raw(format!("unchanged ({})", now(&form.current_mode))),
            };
            let model = match &form.model {
                Some(m) => Span::styled(m.clone(), accent()),
                None => Span::raw(format!("unchanged ({})", now(&form.current_model))),
            };
            let mut lines = vec![
                Line::from(vec![
                    Span::raw(format!(" {}", form.text)),
                    Span::styled("▏", accent()),
                ]),
                Line::from(vec![
                    Span::styled(" mode ", dim()),
                    mode,
                    Span::styled("   model ", dim()),
                    model,
                ]),
            ];
            let note = choices_note(app, &form.adapter, dir).or_else(|| {
                (form.mode.is_some() || form.model.is_some()).then(|| {
                    "applies to this message; the agent's TUI keeps its own selection".into()
                })
            });
            if let Some(n) = note {
                lines.push(Line::from(Span::styled(format!(" {n}"), dim())));
            }
            f.render_widget(Paragraph::new(lines), inner);
            if let Some(p) = &form.picker {
                let label = format!("unchanged ({})", now(&form.current_model));
                draw_picker(f, area, p, &models_for(app, &form.adapter, dir), &label);
            }
        }
        Popup::Start(form) => {
            let rect = centered(area, 90, 21);
            f.render_widget(Clear, rect);
            let block =
                frame_block("start agent  ·  Tab fields · C-e $EDITOR · C-s start · Esc cancel");
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let rows = Layout::vertical([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(5),
            ])
            .split(inner);
            let dir = form.repo_dir(&app.cfg);
            let adapter = form.agent();
            let default_label = default_model_label(app, adapter, dir.as_deref(), &form.mode);
            let focus = |i: usize| if form.focus == i { accent() } else { dim() };
            // name
            let mut name = form.name.clone();
            name.set_block(
                Block::default()
                    .borders(Borders::NONE)
                    .title(Span::styled("name", focus(0))),
            );
            f.render_widget(&name, rows[0]);
            // repo
            let repo_line = Line::from(vec![
                Span::styled("repo  ", focus(1)),
                Span::raw(
                    form.repos
                        .get(form.repo_idx)
                        .cloned()
                        .unwrap_or_else(|| "(none configured)".into()),
                ),
                Span::styled(
                    if form.repos.len() > 1 {
                        "  ← → to change"
                    } else {
                        ""
                    },
                    dim(),
                ),
            ]);
            f.render_widget(Paragraph::new(repo_line), rows[1]);
            let mode_line = Line::from(vec![
                Span::styled("agent ", focus(2)),
                Span::raw(
                    form.agents
                        .get(form.agent_idx)
                        .cloned()
                        .unwrap_or_else(|| "(none enabled)".into()),
                ),
                Span::styled("  ← → to change    ", dim()),
                Span::styled("mode ", focus(3)),
                Span::raw(form.mode.clone()),
                Span::styled("  ← → to change", dim()),
            ]);
            f.render_widget(Paragraph::new(mode_line), rows[2]);
            let model_line = Line::from(vec![
                Span::styled("model ", focus(4)),
                Span::raw(form.model.clone().unwrap_or_else(|| default_label.clone())),
                Span::styled(
                    match choices_note(app, adapter, dir.as_deref()) {
                        Some(n) => format!("  {n}"),
                        None => "  Enter to pick · Backspace = default".into(),
                    },
                    dim(),
                ),
            ]);
            f.render_widget(Paragraph::new(model_line), rows[3]);
            let mut prompt = form.prompt.clone();
            prompt.set_block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(dim())
                    .title(Span::styled("prompt", focus(FOCUS_PROMPT))),
            );
            f.render_widget(&prompt, rows[4]);
            // preview
            let name_s = form.name.lines().join("").trim().to_string();
            let slug = if name_s.is_empty() {
                "wip-xxxx (agent renames)".to_string()
            } else {
                name_s.clone()
            };
            let branch = if name_s.is_empty() {
                "wip/wip-xxxx".to_string()
            } else {
                fleet_core::slug::branch_for(&name_s)
            };
            let wt_dir = form
                .repos
                .get(form.repo_idx)
                .and_then(|n| app.cfg.project(n))
                .map(|p| p.worktrees_dir().display().to_string())
                .unwrap_or_default();
            let preview = format!(" → {wt_dir}/{slug} on {branch}");
            let prect = Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(1),
                width: inner.width,
                height: 1,
            };
            f.render_widget(Paragraph::new(Span::styled(preview, dim())), prect);
            if let Some(p) = &form.picker {
                draw_picker(
                    f,
                    area,
                    p,
                    &models_for(app, adapter, dir.as_deref()),
                    &default_label,
                );
            }
        }
    }
}

/// The model picker, drawn over its form. `none_label` names the `None` entry.
fn draw_picker(f: &mut Frame, area: Rect, p: &Picker, models: &[String], none_label: &str) {
    let items = picker_items(models, &p.filter);
    let rect = centered(area, 70, 22);
    f.render_widget(Clear, rect);
    let block = frame_block("model  ·  type to filter · ↑ ↓ · Enter pick · Esc back");
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    let count = if models.is_empty() {
        "   this agent can't list models: type a name".to_string()
    } else {
        format!("   {} models", models.len())
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" / ", accent()),
            Span::raw(p.filter.clone()),
            Span::styled("▏", accent()),
            Span::styled(count, dim()),
        ])),
        rows[0],
    );
    let h = rows[1].height as usize;
    let start = p.selected.saturating_sub(h.saturating_sub(1));
    let list: Vec<ListItem> = if items.is_empty() {
        vec![ListItem::new(Span::styled(" no match", dim()))]
    } else {
        items
            .iter()
            .enumerate()
            .skip(start)
            .take(h)
            .map(|(i, m)| {
                let style = if i == p.selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(
                    format!(" {}", m.as_deref().unwrap_or(none_label)),
                    style,
                ))
            })
            .collect()
    };
    f.render_widget(List::new(list), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_filters_by_every_word() {
        let models: Vec<String> = [
            "anthropic/claude-opus-5-5",
            "anthropic/claude-sonnet-5",
            "openai/gpt-5.4",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(picker_items(&models, "").len(), 4);
        assert_eq!(picker_items(&models, "")[0], None);
        assert_eq!(
            picker_items(&models, "ANTH opus"),
            vec![Some("anthropic/claude-opus-5-5".to_string())]
        );
        assert!(picker_items(&models, "nothing").is_empty());
        // an adapter without a model list: the typed name is the only entry
        assert_eq!(picker_items(&[], ""), vec![None]);
        assert_eq!(picker_items(&[], " opus "), vec![Some("opus".to_string())]);
    }

    #[test]
    fn cycles() {
        let l = ["plan", "build"].map(String::from).to_vec();
        assert_eq!(cycle(&l, "plan", false), "build");
        assert_eq!(cycle(&l, "build", false), "plan");
        assert_eq!(cycle(&l, "plan", true), "build");
        assert_eq!(cycle(&l, "custom", false), "plan");
    }
}
