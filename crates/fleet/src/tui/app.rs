use super::keys::{Action, Key, Keymap, MenuItem, Resolution, RowKind};
use super::notify::Notifier;
use super::popups::{self, Popup};
use super::tabs::{self, Tab};
use super::util;
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::{execute, terminal};
use fleet_core::agents::{Choices, SendMsg, CHOICES_TTL};
use fleet_core::cmd;
use fleet_core::model::Fleet;
use fleet_core::{tmux, Config, Engine};
use futures::StreamExt;
use ratatui::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Severity,
    Activity,
    Name,
}

pub struct App {
    pub cfg: Config,
    pub engine: Arc<Engine>,
    pub keymap: Keymap,
    pub fleet: Fleet,
    pub tab: Tab,
    pub tabs: tabs::TabStates,
    pub pending: Vec<Key>,
    pub menu: Option<Vec<MenuItem>>,
    pub popup: Option<Popup>,
    pub status_line: Option<(String, bool)>,
    pub show_detail: bool,
    pub sort: SortMode,
    pub loading: bool,
    pub quit: bool,
    /// mtime of this process's binary at startup; the header warns when a newer one lands.
    pub started_exe: Option<std::time::SystemTime>,
    notifier: Notifier,
    tx: mpsc::UnboundedSender<Msg>,
    /// Modes/models per (adapter, directory), loaded in the background when a popup needs
    /// them, with the time they arrived. `Err` = discovery failed (shown in the popup, and
    /// retried the next time a popup asks).
    pub choices: HashMap<ChoicesKey, (Instant, std::result::Result<Choices, String>)>,
    /// Keys being loaded right now (no duplicate discovery while one runs).
    choices_loading: HashSet<ChoicesKey>,
}

/// `(adapter name, directory)`: what a mode/model list depends on.
pub type ChoicesKey = (String, PathBuf);

pub enum Msg {
    Snapshot(Fleet),
    Status(String, bool),
    Choices(ChoicesKey, std::result::Result<Choices, String>),
    /// Suspend the TUI, run the closure (blocking, owns the terminal), then resume.
    Suspend(Box<dyn FnOnce() -> Result<()> + Send>),
    /// A close plan computed in the background (git reads): open its confirm popup.
    ClosePlan(Box<cmd::close::ClosePlan>),
    /// A gc plan computed in the background: open the gc popup.
    GcPlan(cmd::gc::GcPlan),
}

pub async fn run(cfg: Config, project: Option<String>) -> Result<()> {
    let keymap = Keymap::from_config(&cfg, project.as_deref().and_then(|p| cfg.project(p)));
    for e in &keymap.overrides_errors {
        eprintln!("fleet: {e}");
    }
    let engine = Arc::new(Engine::new(cfg.clone()));
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    // Background refresh loop.
    {
        let engine = engine.clone();
        let tx = tx.clone();
        let every = Duration::from_secs(cfg.defaults.refresh_secs.max(1));
        tokio::spawn(async move {
            loop {
                let f = engine.snapshot(false).await;
                if tx.send(Msg::Snapshot(f)).is_err() {
                    break;
                }
                tokio::time::sleep(every).await;
            }
        });
    }

    let project_names: Vec<String> = cfg.projects.iter().map(|p| p.name.clone()).collect();
    let startup_popup = if cfg.warnings.is_empty() && keymap.overrides_errors.is_empty() {
        None
    } else {
        let mut ws = cfg.warnings.clone();
        ws.extend(keymap.overrides_errors.iter().cloned());
        Some(Popup::ConfigWarnings(ws))
    };
    let mut app = App {
        notifier: Notifier::new(cfg.defaults.notify),
        cfg,
        engine,
        keymap,
        fleet: Fleet::default(),
        tab: Tab::Agents,
        tabs: {
            let mut t = tabs::TabStates {
                project,
                ..Default::default()
            };
            t.unfold_projects(&project_names);
            t
        },
        pending: vec![],
        menu: None,
        popup: None,
        status_line: None,
        show_detail: true,
        sort: SortMode::Severity,
        loading: true,
        quit: false,
        started_exe: util::exe_mtime(),
        tx,
        choices: HashMap::new(),
        choices_loading: HashSet::new(),
    };
    app.popup = startup_popup;

    let mut term = enter()?;
    let mut events = EventStream::new();
    let res = loop {
        term.draw(|f| tabs::draw(f, &mut app))?;
        tokio::select! {
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => {
                        if let Err(e) = app.on_key(Key::from_event(&k), &k).await { app.set_status(format!("{e:#}"), true); }
                    }
                    // Bracketed paste: the whole clipboard in one event. Without it a paste
                    // arrives as keystrokes and every newline is an Enter, which the start
                    // popup's name field and the message popup both treat as "submit/next".
                    Some(Ok(Event::Paste(text))) => app.on_paste(text),
                    Some(Ok(Event::Resize(..))) => {}
                    Some(Err(e)) => break Err(e.into()),
                    None => break Ok(()),
                    _ => {}
                }
            }
            msg = rx.recv() => {
                match msg {
                    Some(Msg::Snapshot(f)) => { app.notifier.observe(&f); app.fleet = f; app.loading = false; }
                    Some(Msg::Status(s, err)) => app.set_status(s, err),
                    Some(Msg::ClosePlan(p)) => { app.status_line = None; app.popup = Some(Popup::close(p)); }
                    Some(Msg::GcPlan(p)) => { app.status_line = None; app.popup = Some(Popup::gc(p)); }
                    Some(Msg::Choices(key, c)) => {
                        app.choices_loading.remove(&key);
                        app.choices.insert(key, (Instant::now(), c));
                    }
                    Some(Msg::Suspend(work)) => {
                        leave(&mut term)?;
                        let r = work();
                        term = enter()?;
                        if let Err(e) = r { app.set_status(format!("{e:#}"), true); }
                        app.request_refresh();
                    }
                    None => break Ok(()),
                }
            }
        }
        if app.quit {
            break Ok(());
        }
    };
    leave(&mut term)?;
    res
}

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> Result<Term> {
    terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(
        out,
        terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    )?;
    let term = Terminal::new(CrosstermBackend::new(out))?;
    Ok(term)
}

fn leave(term: &mut Term) -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
        terminal::LeaveAlternateScreen
    )?;
    term.show_cursor()?;
    Ok(())
}

impl App {
    pub fn set_status(&mut self, s: String, err: bool) {
        self.status_line = Some((s, err));
    }

    /// A paste goes to the open popup's text field; outside a popup it means nothing.
    pub fn on_paste(&mut self, text: String) {
        if let Some(p) = self.popup.take() {
            self.popup = Some(popups::paste(p, &text));
        }
    }

    /// Load the modes/models `adapter` offers in `dir` unless a fresh answer is already here.
    /// A failed load, or one older than [`CHOICES_TTL`], is loaded again; the old answer
    /// stays on screen until the new one arrives.
    pub fn want_choices(&mut self, adapter: &str, dir: PathBuf) {
        let key: ChoicesKey = (adapter.to_string(), dir);
        if !choices_stale(self.choices.get(&key), Instant::now())
            || !self.choices_loading.insert(key.clone())
        {
            return;
        }
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = match engine.agent_adapter(&key.0) {
                Some(a) => a.choices(&key.1).await.map_err(|e| format!("{e:#}")),
                None => Err(format!("agent `{}` is not enabled", key.0)),
            };
            let _ = tx.send(Msg::Choices(key, r));
        });
    }

    /// Loaded choices for `(adapter, dir)`: `None` = still loading.
    pub fn choices_for(
        &self,
        adapter: &str,
        dir: &std::path::Path,
    ) -> Option<&std::result::Result<Choices, String>> {
        self.choices
            .get(&(adapter.to_string(), dir.to_path_buf()))
            .map(|(_, c)| c)
    }

    pub fn request_refresh(&self) {
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let f = engine.snapshot(false).await;
            let _ = tx.send(Msg::Snapshot(f));
        });
    }

    fn spawn<F>(&self, label: &'static str, fut: F)
    where
        F: std::future::Future<Output = Result<String>> + Send + 'static,
    {
        let tx = self.tx.clone();
        let engine = self.engine.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Status(format!("{label}…"), false));
            match fut.await {
                Ok(s) => {
                    let _ = tx.send(Msg::Status(s, false));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Status(format!("{label}: {e:#}"), true));
                }
            }
            let f = engine.snapshot(false).await;
            let _ = tx.send(Msg::Snapshot(f));
        });
    }

    async fn on_key(&mut self, key: Key, raw: &crossterm::event::KeyEvent) -> Result<()> {
        self.status_line = None;
        // Popups own the keyboard.
        if let Some(p) = self.popup.take() {
            match popups::handle(p, raw, self) {
                popups::Outcome::Keep(p) => self.popup = Some(p),
                popups::Outcome::Close => {}
                popups::Outcome::Submit(cmd) => self.run_popup_cmd(cmd).await?,
            }
            return Ok(());
        }
        if key.code == crossterm::event::KeyCode::Esc {
            self.pending.clear();
            self.menu = None;
            self.tabs.state_mut(self.tab).filter.clear();
            return Ok(());
        }
        self.pending.push(key);
        let kind = self.row_kind();
        match self.keymap.resolve(&self.pending, kind) {
            Resolution::Exact(a) => {
                self.pending.clear();
                self.menu = None;
                self.act(a).await?;
            }
            Resolution::Prefix(menu) => self.menu = Some(menu),
            Resolution::None => {
                self.pending.clear();
                self.menu = None;
            }
        }
        Ok(())
    }

    fn act(
        &mut self,
        a: Action,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + '_>> {
        Box::pin(self.act_inner(a))
    }

    async fn act_inner(&mut self, a: Action) -> Result<()> {
        use Action::*;
        let count = self.row_count();
        let st = self.tabs.state_mut(self.tab);
        match a {
            Quit => self.quit = true,
            Help => self.popup = Some(Popup::help(self)),
            Refresh => self.request_refresh(),
            RefreshReviews => {
                let engine = self.engine.clone();
                self.spawn("refresh reviews", async move {
                    engine.refresh_reviews().await;
                    Ok("reviews refreshed".into())
                });
            }
            Filter => self.popup = Some(Popup::filter(st.filter.clone())),
            Command => self.popup = Some(Popup::command()),
            Inspect => self.show_detail = !self.show_detail,
            Up => st.up(1),
            Down => st.down(1, count),
            Top => st.selected = 0,
            Bottom => st.selected = count.saturating_sub(1),
            PageUp => st.up(10),
            PageDown => st.down(10, count),
            Tab1 => self.tab = Tab::Agents,
            Tab2 => self.tab = Tab::Tmux,
            Tab3 => self.tab = Tab::Worktrees,
            Tab4 => self.tab = Tab::Reviews,
            NextTab => self.tab = self.tab.next(),
            PrevTab => self.tab = self.tab.prev(),
            Collapse => tabs::collapse(self),
            Expand => tabs::expand(self),
            FoldAll => tabs::fold_all(self),
            UnfoldAll => tabs::unfold_all(self),
            ProjectNext => self.cycle_project(1),
            ProjectPrev => self.cycle_project(-1),
            Sort => {
                self.sort = match self.sort {
                    SortMode::Severity => SortMode::Activity,
                    SortMode::Activity => SortMode::Name,
                    SortMode::Name => SortMode::Severity,
                };
                self.set_status(format!("sort: {:?}", self.sort), false);
            }
            Primary => self.primary().await?,
            Open | AgentOpenPlan | ReviewOpen | WorktreeOpenReview => {
                let url = if matches!(a, ReviewOpen | WorktreeOpenReview) {
                    self.selected_review_url()
                } else {
                    self.selected_url()
                };
                match url {
                    Some(u) => {
                        util::open_url(&u)?;
                        self.set_status(format!("opened {u}"), false)
                    }
                    None => self.set_status("nothing to open".into(), true),
                }
            }
            Yank | AgentCopyPlan | ReviewCopy => {
                let url = if matches!(a, ReviewCopy) {
                    self.selected_review_url()
                } else {
                    self.selected_url()
                };
                match url {
                    Some(u) => {
                        util::copy_to_clipboard(&u)?;
                        self.set_status(format!("copied {u}"), false)
                    }
                    None => self.set_status("nothing to copy".into(), true),
                }
            }
            GotoAgents | ReviewGotoAgents => self.goto(Tab::Agents),
            GotoTmux => self.goto(Tab::Tmux),
            GotoWorktree | ReviewGotoWorktrees => self.goto(Tab::Worktrees),
            GotoReview => self.goto(Tab::Reviews),
            AgentAttach => self.attach_selected().await?,
            Task(name) => self.run_task(&name).await?,
            FindNext => self.find(1),
            FindPrev => self.find(-1),
            FilterProject => match self.selected_project_name() {
                Some(p) => self.set_project(Some(p)),
                None => self.set_status("no project on this row".into(), true),
            },
            AgentSend => {
                if let Some(row) = self.selected_send_target() {
                    self.want_choices(&row.adapter, row.cwd.clone());
                    self.popup = Some(Popup::message(self.selected_name(), row));
                } else {
                    self.set_status("no running agent on this row".into(), true);
                }
            }
            AgentRename => {
                let name = self.selected_name();
                self.popup = Some(Popup::rename(name));
            }
            AgentStop => {
                let name = self.selected_name();
                self.popup = Some(Popup::confirm(
                    format!("Stop the agent in `{name}`? (worktree is kept)"),
                    popups::PopupCmd::Stop(name),
                ));
            }
            WorktreeShell => {
                if let Some(w) = self.selected_worktree() {
                    let path = w.path.clone();
                    let session = self.selected_session().unwrap_or_else(|| w.name.clone());
                    let has = tmux::has_session(&session).await;
                    let sess = session.clone();
                    self.spawn("shell", async move {
                        if has {
                            tmux::new_window(&sess, &path).await?;
                        } else {
                            tmux::new_session(&sess, &path).await?;
                        }
                        tmux::attach(&sess).await?;
                        Ok(format!("shell in {sess}"))
                    });
                }
            }
            WorktreePush => {
                if let Some(w) = self.selected_worktree() {
                    let (path, branch) = (w.path.clone(), w.branch.clone());
                    match branch {
                        Some(b) => self.spawn("push", async move {
                            fleet_core::worktrees::git::push_set_upstream(&path, &b).await?;
                            Ok(format!("pushed {b}"))
                        }),
                        None => self.set_status("detached worktree: nothing to push".into(), true),
                    }
                }
            }
            WorktreePull => {
                if let Some(w) = self.selected_worktree() {
                    let path = w.path.clone();
                    self.spawn("pull", async move {
                        fleet_core::worktrees::git::pull_ff(&path).await?;
                        Ok("pulled".into())
                    });
                }
            }
            WorktreeClose => self.plan_close(),
            WorktreeGc => {
                let (f, cfg, tx) = (self.fleet.clone(), self.cfg.clone(), self.tx.clone());
                self.set_status("gc: planning…".into(), false);
                tokio::spawn(async move {
                    let _ = tx.send(Msg::GcPlan(cmd::gc::plan(&f, &cfg).await));
                });
            }
            WorktreeColumns => {
                self.tabs.show_paths = !self.tabs.show_paths;
            }
            ReviewStart => {
                if let Some(r) = self.selected_review() {
                    self.start_review(r.display_id.clone());
                }
            }
            ReviewFilter => {
                let s = self.tabs.state_mut(Tab::Reviews);
                s.preset = (s.preset + 1) % tabs::reviews::PRESETS.len();
                let name = tabs::reviews::PRESETS[s.preset];
                self.set_status(format!("filter: {name}"), false);
            }
            TmuxKill => {
                if let Some(s) = self.selected_session() {
                    self.popup = Some(Popup::confirm(
                        format!("Kill tmux session `{s}`?"),
                        popups::PopupCmd::KillTmux(s),
                    ));
                }
            }
            TmuxRename => {
                if let Some(s) = self.selected_session() {
                    self.popup = Some(Popup::tmux_rename(s));
                }
            }
        }
        Ok(())
    }

    async fn run_popup_cmd(&mut self, c: popups::PopupCmd) -> Result<()> {
        use popups::PopupCmd::*;
        match c {
            SetFilter(s) => self.tabs.state_mut(self.tab).filter = s,
            SetProject(p) => self.set_project(p),
            StartReview(id) => self.start_review(id),
            RunCommand(line) => {
                // `:project [name]` is handled in-process (it is TUI state, not a subcommand).
                let words = shell_words::split(&line)?;
                if words.first().map(String::as_str) == Some("project") {
                    match words.get(1) {
                        Some(n) if n == "all" => self.set_project(None),
                        Some(n) => self.set_project(Some(n.clone())),
                        None => {
                            self.popup = Some(Popup::project(&self.cfg, self.tabs.project.clone()))
                        }
                    }
                    return Ok(());
                }
                // `:agent send lev8 hi` -> run `fleet agent send lev8 hi` as a child process,
                // suspended so interactive commands (attach, $EDITOR) work.
                let exe = std::env::current_exe()?;
                let args = words;
                let _ = self.tx.send(Msg::Suspend(Box::new(move || {
                    let st = std::process::Command::new(exe).args(&args).status()?;
                    if !st.success() {
                        anyhow::bail!("`fleet {line}` exited with {st}");
                    }
                    Ok(())
                })));
            }
            Start {
                task,
                name,
                prompt,
                repo,
                agent,
                mode,
                model,
            } => {
                let engine = self.engine.clone();
                self.spawn("start", async move {
                    let out = cmd::start::start(
                        &engine,
                        cmd::start::StartOpts {
                            task: Some(task),
                            name,
                            prompt,
                            repo,
                            agent,
                            mode,
                            model,
                            skills: None,
                            base: None,
                            left_pane: None,
                            attach: false,
                        },
                        &cmd::quiet(),
                    )
                    .await?;
                    Ok(format!(
                        "started {} on {} (tmux {})",
                        out.slug, out.branch, out.tmux_session
                    ))
                });
            }
            Message {
                name,
                text,
                mode,
                model,
            } => {
                let target = self.selected_send_target();
                let engine = self.engine.clone();
                match target {
                    Some(row) => self.spawn("send", async move {
                        let a = engine.agent_adapter(&row.adapter).ok_or_else(|| {
                            anyhow::anyhow!("agent `{}` is not enabled", row.adapter)
                        })?;
                        let what = [mode.as_deref(), model.as_deref()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                        let msg = SendMsg { text, model, mode };
                        cmd::send::check_override(a, &row.cwd, &msg, &cmd::quiet()).await?;
                        a.send(&row, &msg).await?;
                        Ok(if what.is_empty() {
                            format!("sent to {name}")
                        } else {
                            format!("sent to {name} ({what})")
                        })
                    }),
                    None => self.set_status("no running agent".into(), true),
                }
            }
            Rename { old, new } => {
                let cfg = self.cfg.clone();
                self.spawn("rename", async move {
                    let r = cmd::rename::rename(&cfg, &old, &new, &cmd::quiet()).await?;
                    Ok(format!("renamed to {}", r.slug))
                });
            }
            Stop(name) => {
                let f = self.fleet.clone();
                self.spawn("stop", async move {
                    cmd::send::stop(&f, &name).await?;
                    Ok(format!("stopped {name}"))
                });
            }
            KillTmux(s) => self.spawn("kill", async move {
                tmux::kill_session(&s).await?;
                Ok(format!("killed {s}"))
            }),
            TmuxRename { old, new } => self.spawn("rename", async move {
                tmux::rename_session(&old, &new).await?;
                Ok(format!("renamed {old} -> {new}"))
            }),
            Close(plan) => {
                let engine = self.engine.clone();
                self.spawn("close", async move {
                    let fresh = engine.snapshot(false).await;
                    let opts = cmd::close::CloseOpts { force: plan.force };
                    let rep = cmd::close::apply_real(&plan, &fresh, &opts, &cmd::quiet()).await?;
                    if !rep.ok() {
                        anyhow::bail!("{}", rep.failure_lines().join(" · "));
                    }
                    Ok(format!("closed {}", plan.target.name))
                });
            }
            ApplyGc(plan) => {
                let (engine, cfg) = (self.engine.clone(), self.cfg.clone());
                self.spawn("gc", async move {
                    let fresh = engine.snapshot(false).await;
                    let res = cmd::gc::apply(&plan, &fresh, &cfg, &cmd::quiet()).await;
                    let failed: Vec<_> = res.iter().filter(|r| !r.ok()).collect();
                    if !failed.is_empty() {
                        anyhow::bail!(
                            "{} of {} not done (each rolled back or skipped): {}",
                            failed.len(),
                            res.len(),
                            failed
                                .iter()
                                .flat_map(|r| r.failure_lines().into_iter().take(1))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        );
                    }
                    Ok(format!("gc: {} items applied", res.len()))
                });
            }
        }
        Ok(())
    }

    /// `w x`: close the task of the selected row (worktree, agent, session or pane). The plan
    /// needs git reads, so it is built in the background and opens the popup when ready.
    fn plan_close(&mut self) {
        let Some(n) = tabs::selected(self) else {
            return;
        };
        if let tabs::NodeData::Review(v) = &n.data {
            if v.worktrees.len() > 1 {
                self.set_status(
                    format!(
                        "{} has {} local worktrees: unfold it (l) and select one",
                        v.review.display_id,
                        v.worktrees.len()
                    ),
                    true,
                );
                return;
            }
        }
        // A worktree row (or anything on one); else a tmux session/pane whose folder is
        // gone: close resolves its leftovers (branch, session, registry task) by name.
        let target = match (n.worktree().cloned(), &n.data) {
            (Some(w), _) => Ok(w),
            (None, tabs::NodeData::Session { name, panes, .. })
                if panes.iter().any(|p| !p.pane.cwd.exists()) =>
            {
                Err(name.clone())
            }
            (None, tabs::NodeData::Pane(p)) if !p.pane.cwd.exists() => Err(p.pane.session.clone()),
            _ => {
                self.set_status("no worktree on this row: nothing to close".into(), true);
                return;
            }
        };
        let label = match &target {
            Ok(w) => w.name.clone(),
            Err(s) => s.clone(),
        };
        let (f, cfg, tx) = (self.fleet.clone(), self.cfg.clone(), self.tx.clone());
        self.set_status(format!("close {label}: planning…"), false);
        tokio::spawn(async move {
            let r = async {
                let resolved = match target {
                    Ok(w) => {
                        let project = cfg
                            .project(&w.repo)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("project {} not in config", w.repo))?;
                        cmd::close::Resolved::worktree(project, w.path.clone())
                    }
                    Err(session) => cmd::close::resolve(&f, &cfg, &session, None).await?,
                };
                let facts = cmd::close::gather(&resolved, &f).await?;
                cmd::close::plan(&facts, &cmd::close::CloseOpts::default())
            }
            .await;
            let _ = tx.send(match r {
                Ok(p) => Msg::ClosePlan(Box::new(p)),
                Err(e) => Msg::Status(format!("close: {e:#}"), true),
            });
        });
    }

    // ───── selection helpers (delegated to the tab modules) ─────

    pub fn row_count(&self) -> usize {
        tabs::row_count(self)
    }

    fn goto(&mut self, to: Tab) {
        let keys = tabs::selected_keys(self);
        self.tab = to;
        if !keys.is_empty() {
            tabs::select_by_keys(self, &keys);
        }
    }

    /// Start (or re-run) a review agent for a review id like `!514` / `#12`.
    pub fn start_review(&mut self, id: String) {
        self.start_review_task(id, None)
    }

    pub fn start_review_task(&mut self, id: String, task: Option<String>) {
        let num: String = id.trim_start_matches(['!', '#']).to_string();
        let source = self.selected_review().map(|r| r.source).or_else(|| {
            self.fleet
                .reviews
                .iter()
                .find(|r| r.review.display_id == id)
                .map(|r| r.review.source.clone())
        });
        let repo = source.and_then(|s| {
            self.cfg
                .projects
                .iter()
                .find(|x| x.reviews.as_deref() == Some(s.as_str()))
                .map(|x| x.name.clone())
        });
        let engine = self.engine.clone();
        self.spawn("review", async move {
            let out = cmd::review::start(
                &engine,
                cmd::review::ReviewOpts {
                    id_or_url: num,
                    task,
                    repo,
                    agent: None,
                    skills: None,
                    attach: false,
                },
                &cmd::quiet(),
            )
            .await?;
            Ok(match out {
                cmd::review::ReviewOutcome::Started { slug, .. } => format!("started {slug}"),
                cmd::review::ReviewOutcome::ReReviewed { slug, .. } => {
                    format!("re-review sent to {slug}")
                }
            })
        });
    }

    /// `p` / `P`: project1 → project2 → … → all.
    fn cycle_project(&mut self, dir: i32) {
        let names: Vec<Option<String>> = self
            .cfg
            .projects
            .iter()
            .map(|p| Some(p.name.clone()))
            .chain(std::iter::once(None))
            .collect();
        if names.len() <= 1 {
            return;
        }
        let cur = names
            .iter()
            .position(|n| *n == self.tabs.project)
            .unwrap_or(names.len() - 1) as i32;
        let next = (cur + dir).rem_euclid(names.len() as i32) as usize;
        self.set_project(names[next].clone());
    }

    pub fn set_project(&mut self, p: Option<String>) {
        if let Some(n) = &p {
            if self.cfg.project(n).is_none() {
                self.set_status(format!("unknown project `{n}`"), true);
                return;
            }
        }
        self.tabs.project = p;
        for t in Tab::ALL {
            self.tabs.state_mut(t).selected = 0;
        }
        self.rebuild_keymap();
        self.set_status(
            format!("project: {}", self.tabs.project.as_deref().unwrap_or("all")),
            false,
        );
    }

    /// `Enter`, per node kind.
    fn primary(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + '_>> {
        Box::pin(async move {
            let Some(n) = tabs::selected(self) else {
                return Ok(());
            };
            match &n.data {
                tabs::NodeData::Project { .. } => {
                    if n.unfolded {
                        tabs::collapse(self)
                    } else {
                        tabs::expand(self)
                    }
                    Ok(())
                }
                tabs::NodeData::Agent { .. }
                | tabs::NodeData::Session { .. }
                | tabs::NodeData::Pane(_) => self.attach_selected().await,
                tabs::NodeData::Worktree(v) => {
                    if v.agents.is_empty() && n.pane_target().is_none() {
                        self.act(Action::WorktreeShell).await
                    } else {
                        self.attach_selected().await
                    }
                }
                tabs::NodeData::Review(v) => {
                    if n.has_children {
                        if !n.unfolded {
                            tabs::expand(self);
                        } else {
                            self.attach_selected().await?;
                        }
                        Ok(())
                    } else {
                        let id = v.review.display_id.clone();
                        self.popup = Some(Popup::confirm(
                            format!(
                                "Start a review agent for {id}? (detached worktree + tmux session)"
                            ),
                            popups::PopupCmd::StartReview(id),
                        ));
                        Ok(())
                    }
                }
            }
        })
    }

    async fn attach_selected(&mut self) -> Result<()> {
        let Some(target) = tabs::selected_pane_target(self) else {
            self.set_status("no tmux pane on this row".into(), true);
            return Ok(());
        };
        if tmux::inside_tmux() {
            tmux::attach(&target).await?;
            self.set_status(format!("switched to {target}"), false);
        } else {
            let _ = self.tx.send(Msg::Suspend(Box::new(move || {
                let rt = tokio::runtime::Handle::try_current();
                match rt {
                    Ok(h) => h.block_on(tmux::attach(&target)),
                    Err(_) => tokio::runtime::Runtime::new()?.block_on(tmux::attach(&target)),
                }
            })));
        }
        Ok(())
    }

    pub fn selected_url(&self) -> Option<String> {
        tabs::selected_plan_url(self).or_else(|| self.selected_review_url())
    }

    pub fn selected_review_url(&self) -> Option<String> {
        self.selected_review().map(|r| r.url.clone())
    }

    /// What the leader menu is about: the selected row's kind.
    pub fn row_kind(&self) -> RowKind {
        match tabs::selected(self).map(|n| n.data) {
            Some(tabs::NodeData::Agent { .. }) => RowKind::Agent,
            Some(tabs::NodeData::Worktree(_)) => RowKind::Worktree,
            Some(tabs::NodeData::Review(_)) => RowKind::Review,
            Some(tabs::NodeData::Session { .. }) | Some(tabs::NodeData::Pane(_)) => RowKind::Tmux,
            _ => RowKind::Other,
        }
    }

    /// The project the selected row belongs to, if any.
    pub fn selected_project_name(&self) -> Option<String> {
        let n = tabs::selected(self)?;
        match &n.data {
            tabs::NodeData::Project { name, .. } => Some(name.clone()),
            tabs::NodeData::Agent { project, .. } => project.clone(),
            tabs::NodeData::Review(v) => self
                .cfg
                .projects
                .iter()
                .find(|p| p.reviews.as_deref() == Some(v.review.source.as_str()))
                .map(|p| p.name.clone()),
            _ => n.worktree().map(|w| w.repo.clone()),
        }
        .filter(|name| self.cfg.project(name).is_some())
    }

    /// The keymap depends on the project scope (per-project keys and tasks).
    fn rebuild_keymap(&mut self) {
        let project = self
            .tabs
            .project
            .as_deref()
            .and_then(|p| self.cfg.project(p));
        self.keymap = Keymap::from_config(&self.cfg, project);
        for e in &self.keymap.overrides_errors.clone() {
            self.set_status(e.clone(), true);
        }
    }

    /// `f` / `F`: the next / previous row that matches the filter text; with no filter, the
    /// next / previous row that needs you.
    fn find(&mut self, dir: i32) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let st = self.tabs.state(self.tab);
        let filter = st.filter.to_lowercase();
        let start = st.selected;
        let matches = |i: usize| -> bool {
            let Some(n) = tabs::node_at(self, i) else {
                return false;
            };
            if filter.is_empty() {
                n.agents().iter().any(|a| a.activity.is_waiting())
            } else {
                n.name().to_lowercase().contains(&filter)
            }
        };
        for step in 1..=count {
            let i = (start as i64 + dir as i64 * step as i64).rem_euclid(count as i64) as usize;
            if matches(i) {
                self.tabs.state_mut(self.tab).selected = i;
                return;
            }
        }
        self.set_status(
            if filter.is_empty() {
                "no row needs you".into()
            } else {
                format!("no other row matches `{}`", st.filter)
            },
            false,
        );
    }

    /// Start a task from config on the selected row: `project` tasks open the start popup
    /// prefilled; `review` tasks start on the selected review.
    async fn run_task(&mut self, name: &str) -> Result<()> {
        let project = self
            .selected_project_name()
            .or_else(|| self.tabs.project.clone())
            .and_then(|p| self.cfg.project(&p).cloned());
        let task = match self.cfg.task(name, project.as_ref()) {
            Ok(t) => t,
            Err(e) => {
                self.set_status(format!("task {name}: {e:#}"), true);
                return Ok(());
            }
        };
        match task.on {
            fleet_core::config::On::Review => match self.selected_review() {
                Some(r) => self.start_review_task(r.display_id.clone(), Some(name.to_string())),
                None => self.set_status(
                    format!("task {name} runs on a review: select an MR/PR first"),
                    true,
                ),
            },
            fleet_core::config::On::Project => {
                let p = Popup::start_task(&self.cfg, project.as_ref(), &task);
                if let Popup::Start(f) = &p {
                    if let Some(dir) = f.repo_dir(&self.cfg) {
                        let agent = f.agent().to_string();
                        self.want_choices(&agent, dir);
                    }
                }
                self.popup = Some(p);
            }
        }
        Ok(())
    }

    pub fn selected_review(&self) -> Option<fleet_core::model::ReviewRow> {
        tabs::selected_review(self)
    }

    pub fn selected_worktree(&self) -> Option<fleet_core::model::WorktreeRow> {
        tabs::selected_worktree(self)
    }

    pub fn selected_session(&self) -> Option<String> {
        tabs::selected_pane_target(self).map(|t| t.split(':').next().unwrap_or(&t).to_string())
    }

    pub fn selected_name(&self) -> String {
        tabs::selected_name(self)
    }

    pub fn selected_send_target(&self) -> Option<fleet_core::model::AgentRow> {
        tabs::selected_send_target(self)
    }
}

/// Does a cached mode/model answer need loading (again)? Missing, failed, or older than
/// [`CHOICES_TTL`].
fn choices_stale(
    entry: Option<&(Instant, std::result::Result<Choices, String>)>,
    now: Instant,
) -> bool {
    match entry {
        None => true,
        Some((_, Err(_))) => true,
        Some((at, Ok(_))) => now.duration_since(*at) >= CHOICES_TTL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_reload_on_error_and_after_ttl() {
        let t0 = Instant::now();
        assert!(choices_stale(None, t0));
        assert!(choices_stale(Some(&(t0, Err("timed out".into()))), t0));
        let ok = (t0, Ok(Choices::default()));
        assert!(!choices_stale(Some(&ok), t0));
        assert!(choices_stale(Some(&ok), t0 + CHOICES_TTL));
    }
}
