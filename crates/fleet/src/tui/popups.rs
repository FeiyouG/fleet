use super::app::App;
use super::keys::{Action, Key};
use super::util::{accent, dim};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fleet_core::cmd::close::ClosePlan;
use fleet_core::cmd::gc::GcPlan;
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
        task: String,
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
    /// Close a task; `plan.force` says whether blockers were overridden.
    Close(Box<ClosePlan>),
    ApplyGc(GcPlan),
}

pub enum Popup {
    Help,
    /// Startup: configuration warnings (fleet runs, the listed items will not take effect).
    ConfigWarnings(Vec<String>),
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
    /// Everything `w x` would remove, its blockers, and (once `F` is pressed) force.
    Close(Box<ClosePlan>),
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
    /// The task preset this popup starts (`start` by default).
    pub task: String,
    /// Skill names the task will load (shown in the title; edit them in config).
    pub skills: Vec<String>,
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
    if let Some(m) = app.cfg.agent(adapter, None).and_then(|a| a.model) {
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
    pub fn close(plan: Box<ClosePlan>) -> Popup {
        Popup::Close(plan)
    }
    pub fn gc(plan: GcPlan) -> Popup {
        let n = plan.txns.len();
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
        let project = std::env::current_dir()
            .ok()
            .and_then(|c| cfg.project_for_path(&c).cloned());
        match cfg.task("start", project.as_ref()) {
            Ok(t) => Self::start_task(cfg, project.as_ref(), &t),
            Err(_) => Self::start_blank(cfg),
        }
    }

    /// The start popup prefilled from a task: its agent, mode, model, preset prompt.
    pub fn start_task(
        cfg: &Config,
        project: Option<&fleet_core::config::Project>,
        task: &fleet_core::config::ResolvedTask,
    ) -> Popup {
        let Popup::Start(mut f) = Self::start_blank(cfg) else {
            unreachable!()
        };
        if let Some(p) = project {
            if let Some(i) = f.repos.iter().position(|r| *r == p.name) {
                f.repo_idx = i;
            }
        }
        if let Some(i) = f.agents.iter().position(|a| *a == task.agent.name) {
            f.agent_idx = i;
        }
        f.mode = task.mode.clone();
        f.model = task.model.clone();
        if let Some(p) = &task.prompt {
            f.prompt = TextArea::from(p.lines().map(String::from).collect::<Vec<_>>());
            f.prompt.set_cursor_line_style(Style::default());
        }
        f.task = task.name.clone();
        f.skills = task
            .skills
            .iter()
            .map(|s| fleet_core::config::validate::skill_name(s))
            .collect();
        Popup::Start(f)
    }

    fn start_blank(cfg: &Config) -> Popup {
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
        let agents: Vec<String> = cfg.enabled_agents().into_iter().map(|a| a.name).collect();
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
            task: "start".into(),
            skills: vec![],
        })
    }
}

pub fn handle(p: Popup, ev: &KeyEvent, app: &mut App) -> Outcome {
    let esc = ev.code == KeyCode::Esc;
    match p {
        Popup::Help | Popup::ConfigWarnings(_) => Outcome::Close,
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
        Popup::Close(plan) => {
            let (out, err) = close_keys(plan, ev.code);
            if let Some(e) = err {
                app.set_status(e, true);
            }
            out
        }
        Popup::Gc {
            plan,
            mut checked,
            mut selected,
        } => match ev.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                selected = (selected + 1).min(plan.txns.len().saturating_sub(1));
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
                let txns: Vec<_> = plan
                    .txns
                    .iter()
                    .zip(&checked)
                    .filter(|(_, c)| **c)
                    .map(|(a, _)| a.clone())
                    .collect();
                if txns.is_empty() {
                    Outcome::Close
                } else {
                    Outcome::Submit(PopupCmd::ApplyGc(GcPlan {
                        txns,
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
                    task: form.task.clone(),
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
                // A letter typed on a selector field (repo/agent/mode/model) is prompt text that
                // started before Tab reached the prompt (or a paste in a terminal without
                // bracketed paste): move there and keep it instead of dropping it.
                _ => {
                    if let KeyCode::Char(_) = ev.code {
                        if !ctrl {
                            form.focus = FOCUS_PROMPT;
                            form.prompt.input(*ev);
                        }
                    }
                }
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

/// Line endings as they come from terminals -> `\n`.
pub fn normalise_paste(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Insert pasted text into a textarea as lines, cursor left at its end.
pub fn insert_paste(ta: &mut TextArea<'static>, text: &str) {
    ta.insert_str(normalise_paste(text));
}

/// Where a paste lands. The start popup's prompt takes it whatever field has focus (nobody
/// pastes a repo name); the message popup appends it; one-line inputs take the first line.
pub fn paste(p: Popup, text: &str) -> Popup {
    match p {
        Popup::Start(mut form) => {
            form.focus = FOCUS_PROMPT;
            insert_paste(&mut form.prompt, text);
            Popup::Start(form)
        }
        Popup::Message(mut form) => {
            form.text.push_str(&normalise_paste(text));
            Popup::Message(form)
        }
        Popup::Input {
            title,
            mut value,
            kind,
        } => {
            value.push_str(normalise_paste(text).lines().next().unwrap_or(""));
            Popup::Input { title, value, kind }
        }
        other => other,
    }
}

/// Keys of the close popup (pure, so it is tested). Nothing blocks: `y`/`Enter` closes.
/// Blocked: `F` arms force and lists in red what will be lost; only a second `F` closes
/// (`Enter`/`y` do nothing then, so a reflexive Enter never destroys work). Returns a
/// message for the status line.
fn close_keys(plan: Box<ClosePlan>, code: KeyCode) -> (Outcome, Option<String>) {
    let armed = plan.force && !plan.blockers.is_empty();
    match code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
            (Outcome::Close, None)
        }
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter if !plan.blocked() && !armed => {
            (Outcome::Submit(PopupCmd::Close(plan)), None)
        }
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter if armed => (
            Outcome::Keep(Popup::Close(plan)),
            Some("press F again to close anyway (the red items are lost), Esc to cancel".into()),
        ),
        KeyCode::Char('F') if armed => (Outcome::Submit(PopupCmd::Close(plan)), None),
        KeyCode::Char('F') if plan.blocked() => match plan.forced() {
            Ok(p) => (Outcome::Keep(Popup::Close(Box::new(p))), None),
            Err(e) => (Outcome::Keep(Popup::Close(plan)), Some(format!("{e:#}"))),
        },
        _ => (Outcome::Keep(Popup::Close(plan)), None),
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

/// Body of the close popup: what is removed (with undo), notes, blockers, keys.
fn close_lines(plan: &ClosePlan) -> Vec<Line<'static>> {
    let red = Style::default().fg(Color::Red);
    let yellow = Style::default().fg(Color::Yellow);
    let mut lines = vec![
        Line::from(Span::styled(plan.header(), accent())),
        Line::from(Span::styled("this removes, in this order:", dim())),
    ];
    let w = plan
        .step_lines()
        .iter()
        .map(|(d, _)| d.chars().count())
        .max()
        .unwrap_or(0);
    for (i, (d, u)) in plan.step_lines().into_iter().enumerate() {
        let undo_style = if u.starts_with("cannot") {
            yellow
        } else {
            dim()
        };
        lines.push(Line::from(vec![
            Span::raw(format!(" {:>2} {d:<w$}  ", i + 1)),
            Span::styled(u, undo_style),
        ]));
    }
    for n in &plan.notes {
        lines.push(Line::from(Span::styled(format!(" note: {n}"), dim())));
    }
    if !plan.blockers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            if plan.force {
                "LOST when you press F again:"
            } else {
                "blocked — this would be lost:"
            },
            red.add_modifier(Modifier::BOLD),
        )));
        for l in plan.blocker_lines(10) {
            lines.push(Line::from(Span::styled(format!(" {l}"), red)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if plan.blocked() {
            "F = force (shows what is lost, then F again closes) · Esc cancel"
        } else if plan.force && !plan.blockers.is_empty() {
            "F again = close anyway · Esc cancel"
        } else {
            "Enter / y = close · Esc cancel · all-or-nothing: a failure restores what was done"
        },
        dim(),
    )));
    lines
}

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

pub fn draw_whichkey(f: &mut Frame, area: Rect, pending: &[Key], menu: &[super::keys::MenuItem]) {
    let prefix = pending
        .iter()
        .map(Key::display)
        .collect::<Vec<_>>()
        .join(" ");
    let mut items: Vec<Line> = menu
        .iter()
        .map(|m| {
            let label = match &m.disabled {
                Some(why) => format!("{} ({why})", m.label),
                None => m.label.clone(),
            };
            Line::from(vec![
                Span::styled(format!(" {:<6}", m.key.display()), accent()),
                if m.disabled.is_some() {
                    Span::styled(label, dim())
                } else {
                    Span::raw(label)
                },
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
        Popup::ConfigWarnings(ws) => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "fleet is running, but some configuration will not take effect:",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
            ];
            for w in ws {
                lines.push(Line::from(vec![
                    Span::styled(" ⚠ ", Style::default().fg(Color::Yellow)),
                    Span::raw(w.clone()),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "fix ~/.config/fleet/init.lua and restart · `fleet config --check` · any key to continue"
                ),
                dim(),
            )));
            let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
            let rect = centered(area, 100, h);
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(frame_block("configuration")),
                rect,
            );
        }
        Popup::Help => {
            let mut lines = vec![Line::from(Span::styled(
                "keys (from config: `fleet config --default` shows the shipped map; override in init.lua `keys`)",
                dim(),
            ))];
            let mut group = "";
            for a in Action::ALL {
                let name = a.name();
                let label = a.label();
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
                            " {:<10}",
                            app.keymap.chord_for(a).unwrap_or_else(|| "—".into())
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
                .txns
                .iter()
                .zip(checked)
                .enumerate()
                .map(|(i, (t, c))| {
                    let mark = if *c { "[x]" } else { "[ ]" };
                    let style = if i == *selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    let mut lines = vec![Line::from(Span::styled(
                        format!(" {mark} {}", t.label),
                        style,
                    ))];
                    for s in &t.steps {
                        lines.push(Line::from(Span::styled(
                            format!("       {}", s.describe()),
                            dim(),
                        )));
                    }
                    ListItem::new(Text::from(lines))
                })
                .collect();
            if plan.txns.is_empty() {
                items.push(ListItem::new(Span::styled(" nothing to clean", dim())));
            }
            for (n, r) in &plan.blocked {
                items.push(ListItem::new(Span::styled(
                    format!(" blocked: {n} ({r})"),
                    dim(),
                )));
            }
            items.push(ListItem::new(Span::styled(
                " Space toggle · a all · Enter apply checked (each all-or-nothing) · Esc cancel",
                dim(),
            )));
            let h: usize = items.iter().map(ListItem::height).sum();
            let rect = centered(area, 110, h as u16 + 2);
            f.render_widget(Clear, rect);
            f.render_widget(
                List::new(items).block(frame_block("worktree gc (dry run)")),
                rect,
            );
        }
        Popup::Close(plan) => {
            let lines = close_lines(plan);
            let rect = centered(area, 120, lines.len() as u16 + 2);
            f.render_widget(Clear, rect);
            let (title, border) = if plan.force && !plan.blockers.is_empty() {
                (
                    "FORCE close — the items marked lost go for good",
                    Color::Red,
                )
            } else if plan.blocked() {
                ("close task — blocked", Color::Yellow)
            } else {
                ("close task", Color::Magenta)
            };
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border))
                        .title(Span::styled(
                            format!(" {title} "),
                            Style::default().fg(border),
                        )),
                ),
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
            let text_lines = form.text.split('\n').count();
            let rect = centered(area, 90, (6 + text_lines.saturating_sub(1) as u16).min(20));
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
            // Every line of the text; the cursor sits after the last.
            let mut lines: Vec<Line> = vec![];
            let parts: Vec<&str> = form.text.split('\n').collect();
            for (i, l) in parts.iter().enumerate() {
                let mut spans = vec![Span::raw(format!(" {l}"))];
                if i + 1 == parts.len() {
                    spans.push(Span::styled("▏", accent()));
                }
                lines.push(Line::from(spans));
            }
            lines.extend([Line::from(vec![
                Span::styled(" mode ", dim()),
                mode,
                Span::styled("   model ", dim()),
                model,
            ])]);
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
            // Tall enough to show a pasted multi-paragraph prompt (the box scrolls with the
            // cursor; the title says how many lines it holds).
            let rect = centered(area, 100, area.height.saturating_sub(4).clamp(21, 40));
            f.render_widget(Clear, rect);
            let title = if form.skills.is_empty() {
                format!(
                    "task {}  ·  Tab fields · C-e $EDITOR · C-s start · Esc cancel",
                    form.task
                )
            } else {
                format!(
                    "task {} · skills: {}  ·  Tab fields · C-e $EDITOR · C-s start · Esc cancel",
                    form.task,
                    form.skills.join(", ")
                )
            };
            let block = frame_block(&title);
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
            let n = form.prompt.lines().len();
            let visible = rows[4].height.saturating_sub(1) as usize;
            let title = if n > visible {
                format!("prompt · {n} lines (scrolls)")
            } else if n > 1 {
                format!("prompt · {n} lines")
            } else {
                "prompt".to_string()
            };
            prompt.set_block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(dim())
                    .title(Span::styled(title, focus(FOCUS_PROMPT))),
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
    use fleet_core::cmd::close::{self, CloseOpts, Facts};
    use fleet_core::config::Project;

    #[test]
    fn paste_normalises_line_endings_and_keeps_every_line() {
        assert_eq!(normalise_paste("a\r\nb\rc\n"), "a\nb\nc\n");
        let mut ta = TextArea::default();
        ta.insert_str("typed ");
        insert_paste(&mut ta, "one\r\ntwo\n\nfour");
        assert_eq!(ta.lines(), ["typed one", "two", "", "four"]);
        assert_eq!(ta.cursor(), (3, 4));
    }

    #[test]
    fn paste_into_start_popup_lands_in_prompt_whatever_has_focus() {
        let mut cfg = Config::default();
        cfg.projects.push({
            let mut p = Project::new("svc", "/r");
            p.worktrees = None;
            p.reviews = None;
            p.base = "main".into();
            p
        });
        let Popup::Start(mut form) = Popup::start(&cfg) else {
            panic!("start popup")
        };
        form.focus = 0; // name field
        let p = paste(Popup::Start(form), "Fix the thing.\n\nSecond paragraph.");
        let Popup::Start(form) = p else { panic!() };
        assert_eq!(form.focus, FOCUS_PROMPT);
        assert_eq!(
            form.prompt.lines().join("\n"),
            "Fix the thing.\n\nSecond paragraph."
        );
        assert_eq!(form.name.lines().join(""), "");
    }

    #[test]
    fn paste_into_message_and_input() {
        let m = Popup::message(
            "t".into(),
            fleet_core::model::AgentRow {
                adapter: "opencode".into(),
                pid: 1,
                cwd: "/w".into(),
                session_id: None,
                title: None,
                mode: None,
                model: None,
                activity: fleet_core::model::Activity::Idle,
                running_tool: None,
                plan_url: None,
                plan_title: None,
                port: None,
                last_activity: None,
                pane_target: None,
                pending: vec![],
                control_url: None,
                control_token: None,
            },
        );
        let Popup::Message(f) = paste(m, "a\r\nb") else {
            panic!()
        };
        assert_eq!(f.text, "a\nb");
        let Popup::Input { value, .. } = paste(Popup::command(), "one\ntwo") else {
            panic!()
        };
        assert_eq!(value, "one");
    }

    fn plan(dirty: bool) -> Box<ClosePlan> {
        let f = Facts {
            project: {
                let mut p = Project::new("svc", "/r");
                p.worktrees = None;
                p.reviews = None;
                p.base = "main".into();
                p
            },
            path: Some("/w/feat-x".into()),
            name: "feat-x".into(),
            is_main: false,
            listed: true,
            missing: false,
            branch: Some("feat/x".into()),
            branch_guessed: false,
            head: "89aa2b0aaaaaaaaa".into(),
            upstream: None,
            locked: None,
            dirty: if dirty { vec!["?? x".into()] } else { vec![] },
            only_here: vec![],
            only_here_more: false,
            landed: vec![],
            review_merged: None,
            keep_branch: None,
            tasks: vec![],
            agents: vec![],
            sessions: vec![],
            host_panes: vec![],
            host_agents: vec![],
            notes: vec![],
        };
        Box::new(close::plan(&f, &CloseOpts::default()).unwrap())
    }

    #[test]
    fn clean_plan_closes_on_enter() {
        assert!(matches!(
            close_keys(plan(false), KeyCode::Enter).0,
            Outcome::Submit(PopupCmd::Close(p)) if !p.force
        ));
        assert!(matches!(
            close_keys(plan(false), KeyCode::Esc).0,
            Outcome::Close
        ));
    }

    #[test]
    fn blocked_plan_needs_f_twice() {
        let (out, _) = close_keys(plan(true), KeyCode::Enter);
        let Outcome::Keep(Popup::Close(p)) = out else {
            panic!("Enter on a blocked plan must keep the popup")
        };
        let (out, err) = close_keys(p, KeyCode::Char('F'));
        assert!(err.is_none());
        let Outcome::Keep(Popup::Close(p)) = out else {
            panic!("F arms force")
        };
        assert!(p.force && !p.blocked());
        assert!(close_lines(&p)
            .iter()
            .any(|l| l.to_string().contains("LOST")));
        // Armed: Enter / y do nothing but explain; only a second F closes.
        for k in [KeyCode::Enter, KeyCode::Char('y')] {
            let (out, msg) = close_keys(p.clone(), k);
            assert!(matches!(out, Outcome::Keep(Popup::Close(_))), "{k:?}");
            assert!(msg.unwrap().contains("F again"));
        }
        assert!(matches!(
            close_keys(p, KeyCode::Char('F')).0,
            Outcome::Submit(PopupCmd::Close(p)) if p.force
        ));
    }

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
