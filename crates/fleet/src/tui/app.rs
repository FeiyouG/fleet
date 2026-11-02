use super::keys::{Action, Key, Keymap, Resolution};
use super::notify::Notifier;
use super::popups::{self, Popup};
use super::tabs::{self, Tab};
use super::util;
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::{execute, terminal};
use fleet_core::agents::SendTarget;
use fleet_core::cmd;
use fleet_core::model::Fleet;
use fleet_core::{tmux, Config, Engine};
use futures::StreamExt;
use ratatui::prelude::*;
use std::sync::Arc;
use std::time::Duration;
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
    pub menu: Option<Vec<(Key, Action)>>,
    pub popup: Option<Popup>,
    pub status_line: Option<(String, bool)>,
    pub show_detail: bool,
    pub sort: SortMode,
    pub loading: bool,
    pub quit: bool,
    notifier: Notifier,
    tx: mpsc::UnboundedSender<Msg>,
}

pub enum Msg {
    Snapshot(Fleet),
    Status(String, bool),
    /// Suspend the TUI, run the closure (blocking, owns the terminal), then resume.
    Suspend(Box<dyn FnOnce() -> Result<()> + Send>),
}

pub async fn run(cfg: Config, project: Option<String>) -> Result<()> {
    let keymap = Keymap::from_config(&cfg);
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
        tx,
    };

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
        crossterm::event::EnableMouseCapture
    )?;
    let term = Terminal::new(CrosstermBackend::new(out))?;
    Ok(term)
}

fn leave(term: &mut Term) -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        term.backend_mut(),
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
        match self.keymap.resolve(&self.pending) {
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
            Detail => self.show_detail = !self.show_detail,
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
            Copy | AgentCopyPlan | ReviewCopy => {
                let url = if a == ReviewCopy {
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
            AgentStart => self.popup = Some(Popup::start(&self.cfg)),
            AgentSend => {
                if self.selected_send_target().is_some() {
                    self.popup = Some(Popup::message(self.selected_name()));
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
            WorktreeRemove => {
                if let Some(w) = self.selected_worktree() {
                    let (repo, path) = (w.repo.clone(), w.path.clone());
                    self.popup = Some(Popup::confirm(
                        format!(
                            "Remove worktree {}? (git worktree remove; refuses if dirty)",
                            path.display()
                        ),
                        popups::PopupCmd::RemoveWorktree { repo, path },
                    ));
                }
            }
            WorktreeGc => {
                let plan = cmd::gc::plan(&self.fleet, &self.cfg);
                self.popup = Some(Popup::gc(plan));
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
                name,
                prompt,
                repo,
                mode,
            } => {
                let engine = self.engine.clone();
                self.spawn("start", async move {
                    let out = cmd::start::start(
                        &engine,
                        cmd::start::StartOpts {
                            name,
                            prompt,
                            repo,
                            mode,
                            base: None,
                            nvim: None,
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
            Message { name, text } => {
                let target = self.selected_send_target();
                let engine = self.engine.clone();
                match target {
                    Some(t) => self.spawn("send", async move {
                        let a = engine
                            .agent_adapter("opencode")
                            .ok_or_else(|| anyhow::anyhow!("no opencode adapter"))?;
                        a.send(&t, &text).await?;
                        Ok(format!("sent to {name}"))
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
            RemoveWorktree { repo, path } => {
                let rp = self.cfg.project(&repo).map(|r| r.repo.clone());
                self.spawn("remove", async move {
                    let rp = rp.ok_or_else(|| anyhow::anyhow!("repo {repo} not in config"))?;
                    fleet_core::worktrees::git::worktree_remove(&rp, &path, false).await?;
                    Ok(format!("removed {}", path.display()))
                });
            }
            ApplyGc(plan) => {
                let cfg = self.cfg.clone();
                self.spawn("gc", async move {
                    let res = cmd::gc::apply(&plan, &cfg, &cmd::quiet()).await;
                    let failed = res.iter().filter(|(_, r)| r.is_err()).count();
                    if failed > 0 {
                        anyhow::bail!("{failed} of {} actions failed", res.len());
                    }
                    Ok(format!("gc: {} actions applied", res.len()))
                });
            }
        }
        Ok(())
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
                    repo,
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

    pub fn selected_send_target(&self) -> Option<SendTarget> {
        tabs::selected_send_target(self)
    }
}
