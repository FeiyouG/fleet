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
    },
    Message {
        name: String,
        text: String,
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
    Message(String),
    Rename(String),
    TmuxRename(String),
}

pub struct StartForm {
    pub name: TextArea<'static>,
    pub prompt: TextArea<'static>,
    pub repos: Vec<String>,
    pub repo_idx: usize,
    pub agents: Vec<String>,
    pub agent_idx: usize,
    pub modes: Vec<String>,
    pub mode_idx: usize,
    /// 0 name, 1 repo, 2 agent, 3 mode, 4 prompt
    pub focus: usize,
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
    pub fn message(name: String) -> Popup {
        Popup::Input {
            title: format!("message to {name}"),
            value: String::new(),
            kind: InputKind::Message(name),
        }
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
        let modes = vec!["plan".to_string(), "build".to_string()];
        let mode_idx = modes
            .iter()
            .position(|m| *m == cfg.defaults.mode)
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
            modes,
            mode_idx,
            focus: 4,
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
                        InputKind::Message(name) => {
                            if v.is_empty() {
                                Outcome::Close
                            } else {
                                Outcome::Submit(PopupCmd::Message { name, text: v })
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
                    mode: form.modes.get(form.mode_idx).cloned(),
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
                    (form.focus + 4) % 5
                } else {
                    (form.focus + 1) % 5
                };
                return Outcome::Keep(Popup::Start(form));
            }
            match form.focus {
                0 => {
                    if ev.code != KeyCode::Enter {
                        form.name.input(*ev);
                    } else {
                        form.focus = 4;
                    }
                }
                1 => match ev.code {
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('k') => {
                        form.repo_idx =
                            (form.repo_idx + form.repos.len().max(1) - 1) % form.repos.len().max(1)
                    }
                    KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('j')
                    | KeyCode::Char(' ')
                    | KeyCode::Enter => {
                        form.repo_idx = (form.repo_idx + 1) % form.repos.len().max(1)
                    }
                    _ => {}
                },
                2 => match ev.code {
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('k') => {
                        form.agent_idx = (form.agent_idx + form.agents.len().max(1) - 1)
                            % form.agents.len().max(1)
                    }
                    KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('j')
                    | KeyCode::Char(' ')
                    | KeyCode::Enter => {
                        form.agent_idx = (form.agent_idx + 1) % form.agents.len().max(1)
                    }
                    _ => {}
                },
                3 => match ev.code {
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('h')
                    | KeyCode::Char('l')
                    | KeyCode::Char(' ')
                    | KeyCode::Enter => form.mode_idx = (form.mode_idx + 1) % form.modes.len(),
                    _ => {}
                },
                _ => {
                    form.prompt.input(*ev);
                }
            }
            Outcome::Keep(Popup::Start(form))
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
        Popup::Start(form) => {
            let rect = centered(area, 90, 20);
            f.render_widget(Clear, rect);
            let block =
                frame_block("start agent  ·  Tab fields · C-e $EDITOR · C-s start · Esc cancel");
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let rows = Layout::vertical([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(5),
            ])
            .split(inner);
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
                Span::raw(form.modes[form.mode_idx].clone()),
                Span::styled("  ← → to change", dim()),
            ]);
            f.render_widget(Paragraph::new(mode_line), rows[2]);
            let mut prompt = form.prompt.clone();
            prompt.set_block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(dim())
                    .title(Span::styled("prompt", focus(4))),
            );
            f.render_widget(&prompt, rows[3]);
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
        }
    }
}
