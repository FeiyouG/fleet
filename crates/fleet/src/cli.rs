use crate::tui;
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use fleet_core::cmd::{self, gc, rename, review, send, start};
use fleet_core::model::Fleet;
use fleet_core::{Config, Engine};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "fleet",
    version,
    about = "Track and launch your local coding agents across tmux, worktrees and reviews."
)]
pub struct Cli {
    /// Config file (default: ~/.config/fleet/init.lua)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Start scoped to this project (default: the project containing the cwd)
    #[arg(long, global = true)]
    project: Option<String>,
    #[arg(skip)]
    all_projects: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the TUI (default when no subcommand is given)
    Tui,
    /// One-shot snapshot for scripts and the tmux status bar
    Status {
        #[arg(long)]
        json: bool,
        /// Compact one-liner, e.g. `3 wait · 1 failed · 2 ready`
        #[arg(long)]
        short: bool,
    },
    /// Agents: start, list, attach, send, rename, stop
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Reviews (MRs / PRs): list, start a review agent
    #[command(subcommand)]
    Review(ReviewCmd),
    /// Worktrees: list, gc
    #[command(subcommand)]
    Worktree(WorktreeCmd),
    /// tmux: list sessions/windows/panes with what runs in them
    #[command(subcommand)]
    Tmux(TmuxCmd),
    /// Alias of `agent start`
    Start(StartArgs),
    /// Print the config path and an example config
    Config {
        /// Write the example config if none exists (the cwd's repo becomes the first project)
        #[arg(long)]
        init: bool,
    },
    /// Projects: list, add
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Print every action and its current key binding
    Keys,
    /// Install each enabled agent's hook/plugin so fleet sees permission prompts (and print the tmux snippet)
    Setup {
        /// Uninstall fleet's hooks/plugins from every agent
        #[arg(long)]
        remove: bool,
        /// Report plugin / tools / agent-state status without changing anything
        #[arg(long)]
        check: bool,
        #[arg(long)]
        json: bool,
    },
    /// Hook handler run by Claude Code / Codex (reads the event JSON on stdin). Not for humans.
    #[command(hide = true, name = "agent-hook")]
    AgentHook { agent: String },
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Create a worktree + branch + tmux session and start an agent on a prompt
    Start(StartArgs),
    /// List agents (what the Overview tab shows)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Switch/attach tmux to the agent's session
    Attach { name: String },
    /// Send a message to a running agent (optionally with another mode and/or model)
    Send {
        name: String,
        text: Vec<String>,
        /// Use this mode for the message: plan | build | an opencode agent
        #[arg(long)]
        mode: Option<String>,
        /// Use this model for the message (opencode: provider/model; `fleet agent choices`)
        #[arg(long)]
        model: Option<String>,
    },
    /// List the modes and models an agent can use in a repo
    Choices {
        /// Repo name from config (default: the cwd's repo, else the only repo)
        #[arg(long)]
        repo: Option<String>,
        /// Which agent: opencode | claude | codex (default: defaults.agent)
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Rename a fleet-started task: branch, folder (+symlink), tmux session, registry
    Rename { old: String, new: String },
    /// Kill the agent pane (keeps the worktree)
    Stop { name: String },
}

#[derive(Args, Clone)]
struct StartArgs {
    /// Task name (slug). Omit to get a temp `wip-xxxx` that the agent renames itself.
    name: Option<String>,
    /// Prompt text (else -f FILE, else $EDITOR)
    #[arg(short = 'm', long)]
    message: Option<String>,
    /// Read the prompt from a file (`-` = stdin)
    #[arg(short = 'f', long)]
    file: Option<PathBuf>,
    /// Repo name from config (default: the cwd's repo, else the only repo)
    #[arg(long)]
    repo: Option<String>,
    /// Which agent to launch: opencode | claude | codex (default: defaults.agent)
    #[arg(long)]
    agent: Option<String>,
    /// plan | build, or an opencode agent (`fleet agent choices` lists them)
    #[arg(long)]
    mode: Option<String>,
    /// Model in the agent's naming, e.g. opencode provider/model (default: agents.<name>.model)
    #[arg(long)]
    model: Option<String>,
    /// Base branch (default: repo's `base`, usually main)
    #[arg(long)]
    base: Option<String>,
    /// Do not open an nvim pane
    #[arg(long)]
    no_nvim: bool,
    /// Do not attach after starting
    #[arg(long)]
    no_attach: bool,
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// List open reviews across configured sources
    List {
        #[arg(long)]
        json: bool,
        /// Only this source, e.g. gitlab:monid-ai/monid-services
        #[arg(long)]
        source: Option<String>,
    },
    /// Start (or re-run) a review agent for an MR/PR id or URL
    Start {
        id_or_url: String,
        #[arg(long)]
        repo: Option<String>,
        /// Which agent to launch (default: defaults.agent)
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        no_attach: bool,
    },
}

#[derive(Subcommand)]
enum WorktreeCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove merged/stale worktrees, dead tmux sessions, rename symlinks (dry run unless --yes)
    Gc {
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// List configured projects with worktree and agent counts
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a project to init.lua (name = folder name, reviews from `origin`, base from origin/HEAD)
    Add {
        /// Path inside the repo (default: cwd)
        path: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        /// `gitlab:group/project` or `github:owner/repo`
        #[arg(long)]
        reviews: Option<String>,
        /// Where new worktrees go (default: ~/.local/share/fleet/worktrees/<name>)
        #[arg(long)]
        worktrees: Option<PathBuf>,
        #[arg(long)]
        base: Option<String>,
    },
}

#[derive(Subcommand)]
enum TmuxCmd {
    List {
        #[arg(long)]
        json: bool,
    },
}

fn progress() -> cmd::Progress {
    Box::new(|s| eprintln!("  {s}"))
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = match &cli.config {
        Some(p) => Config::load_from(p)?,
        None => Config::load()?,
    };
    let mut cli = cli;
    if cli.project.as_deref() == Some("all") {
        cli.project = None;
        cli.all_projects = true;
    }
    if let Some(p) = &cli.project {
        if cfg.project(p).is_none() {
            bail!(
                "unknown project `{p}` (configured: {})",
                cfg.projects
                    .iter()
                    .map(|x| x.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(dispatch(cli, cfg))
}

fn cli_config_path(explicit: &Option<PathBuf>) -> PathBuf {
    explicit
        .clone()
        .unwrap_or_else(fleet_core::config::config_path)
}

async fn snapshot(cfg: &Config) -> Fleet {
    Engine::new(cfg.clone()).snapshot(true).await
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

async fn dispatch(cli: Cli, cfg: Config) -> Result<()> {
    let cli_config = cli.config.clone();
    let project = if cli.all_projects {
        None
    } else {
        cli.project.clone().or_else(|| cwd_project_strict(&cfg))
    };
    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Tui => {
            if cfg.projects.is_empty() {
                eprintln!("fleet: no projects configured. Run `fleet config --init` inside a repo, or `fleet project add <path>`.");
            }
            tui::run(cfg, project).await
        }
        Cmd::Status { json, short } => {
            let f = snapshot(&cfg).await;
            if json {
                return print_json(&f);
            }
            let c = f.counts();
            if short {
                println!(
                    "{} wait · {} failed · {} ready · {} agents",
                    c.needs_you, c.failed, c.ready, c.agents
                );
            } else {
                println!(
                    "{} agents · {} worktrees · {} reviews · {} need you · {} failed · {} ready",
                    c.agents, c.worktrees, c.reviews, c.needs_you, c.failed, c.ready
                );
                for w in &f.warnings {
                    println!("warning: {w}");
                }
            }
            Ok(())
        }
        Cmd::Agent(a) => agent(a, cfg, project).await,
        Cmd::Start(args) => agent(AgentCmd::Start(args), cfg, project).await,
        Cmd::Review(r) => match r {
            ReviewCmd::List { json, source } => {
                let f = snapshot(&cfg).await;
                // --project scopes to that project's source; --source overrides.
                let source = source.or_else(|| {
                    project
                        .as_ref()
                        .and_then(|p| cfg.project(p))
                        .and_then(|p| p.reviews.clone())
                });
                let rows: Vec<_> = f
                    .reviews
                    .iter()
                    .filter(|r| {
                        source
                            .as_deref()
                            .map(|s| r.review.source == s)
                            .unwrap_or(true)
                    })
                    .collect();
                if json {
                    return print_json(&rows);
                }
                println!(
                    "{:<7} {:<10} {:<4} {:>4} {:<14} {:<4} {:<4} {}",
                    "ID", "STATUS", "CI", "APPR", "COMMENTS", "WT", "AG", "TITLE"
                );
                for v in rows {
                    let r = &v.review;
                    println!(
                        "{:<7} {:<10} {:<4} {:>4} {:<14} {:<4} {:<4} {}",
                        r.display_id,
                        r.status.label(),
                        r.ci.glyph(),
                        r.approvals.len(),
                        format!(
                            "{} ✔{} ✘{}",
                            r.threads.total, r.threads.resolved, r.threads.unresolved
                        ),
                        v.worktrees.len(),
                        v.agents.len(),
                        r.title
                    );
                }
                for w in &f.warnings {
                    eprintln!("warning: {w}");
                }
                Ok(())
            }
            ReviewCmd::Start {
                id_or_url,
                repo,
                agent,
                no_attach,
            } => {
                let engine = Engine::new(cfg);
                let repo = repo.or(project).or_else(|| cwd_repo(&engine.config));
                let out = review::start(
                    &engine,
                    review::ReviewOpts {
                        id_or_url,
                        repo,
                        agent,
                        attach: !no_attach,
                    },
                    &progress(),
                )
                .await?;
                match out {
                    review::ReviewOutcome::Started { slug, path, port } => {
                        println!("started {slug} at {} (port {port})", path.display())
                    }
                    review::ReviewOutcome::ReReviewed {
                        slug,
                        old_sha,
                        new_sha,
                    } => {
                        println!(
                            "re-review sent to {slug}: {} -> {}",
                            &old_sha[..old_sha.len().min(8)],
                            &new_sha[..new_sha.len().min(8)]
                        )
                    }
                }
                Ok(())
            }
        },
        Cmd::Worktree(w) => match w {
            WorktreeCmd::List { json } => {
                let mut f = snapshot(&cfg).await;
                if let Some(p) = &project {
                    f.worktrees.retain(|w| w.worktree.repo == *p);
                }
                if json {
                    return print_json(&f.worktrees);
                }
                println!(
                    "{:<28} {:<16} {:<30} {:<6} {:<5} {:<3} {:<12} {}",
                    "NAME", "REPO", "BRANCH", "SYNC", "DIRTY", "AG", "REVIEW", "GC?"
                );
                for v in &f.worktrees {
                    let w = &v.worktree;
                    println!(
                        "{:<28} {:<16} {:<30} {:<6} {:<5} {:<3} {:<12} {}",
                        trunc(&w.name, 28),
                        trunc(&w.repo, 16),
                        trunc(w.branch.as_deref().unwrap_or("(detached)"), 30),
                        w.sync_glyph(),
                        if w.dirty { "*" } else { "" },
                        v.agents.len(),
                        v.review
                            .as_ref()
                            .map(|r| format!("{} {}", r.display_id, r.status.label()))
                            .unwrap_or("—".into()),
                        v.gc.label()
                    );
                }
                Ok(())
            }
            WorktreeCmd::Gc { yes, json } => {
                let f = snapshot(&cfg).await;
                let plan = gc::plan(&f, &cfg);
                if json {
                    print_json(&plan)?;
                    if !yes {
                        return Ok(());
                    }
                } else {
                    if plan.actions.is_empty() {
                        println!("nothing to clean");
                    }
                    for a in &plan.actions {
                        println!("  {}", gc::describe(a));
                    }
                    for (n, r) in &plan.blocked {
                        println!("  blocked: {n} ({r})");
                    }
                }
                if plan.actions.is_empty() {
                    return Ok(());
                }
                if !yes {
                    if !std::io::stdin().is_terminal() {
                        println!("(dry run; pass --yes to apply)");
                        return Ok(());
                    }
                    print!("apply {} actions? [y/N] ", plan.actions.len());
                    std::io::stdout().flush()?;
                    let mut s = String::new();
                    std::io::stdin().read_line(&mut s)?;
                    if !matches!(s.trim(), "y" | "Y" | "yes") {
                        println!("aborted");
                        return Ok(());
                    }
                }
                let results = gc::apply(&plan, &cfg, &progress()).await;
                let failed = results.iter().filter(|(_, r)| r.is_err()).count();
                if failed > 0 {
                    bail!("{failed} action(s) failed");
                }
                Ok(())
            }
        },
        Cmd::Tmux(TmuxCmd::List { json }) => {
            let f = snapshot(&cfg).await;
            if json {
                return print_json(&f.panes);
            }
            for v in &f.panes {
                println!(
                    "{:<36} {} {:<10} {:<28} {}",
                    v.pane.target(),
                    if v.pane.attached { "●" } else { "○" },
                    v.pane.command,
                    v.worktree.as_ref().map(|w| w.name.as_str()).unwrap_or("—"),
                    v.agent
                        .as_ref()
                        .map(|a| format!(
                            "{} {}",
                            a.mode.as_deref().unwrap_or("?"),
                            a.activity.label()
                        ))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
        Cmd::Config { init } => {
            let p = fleet_core::config::config_path();
            println!("{}", p.display());
            let detected = match std::env::current_dir() {
                Ok(cwd) => cmd::project::detect(&cwd, None, None, None, None)
                    .await
                    .ok()
                    .into_iter()
                    .collect::<Vec<_>>(),
                Err(_) => vec![],
            };
            if init {
                if p.exists() {
                    bail!("{} already exists", p.display());
                }
                std::fs::create_dir_all(p.parent().unwrap())?;
                std::fs::write(&p, Config::example(&detected))?;
                println!("wrote {} with {} project(s)", p.display(), detected.len());
            } else if !p.exists() {
                println!("(not found; `fleet config --init` writes this)\n");
                print!("{}", Config::example(&detected));
            }
            Ok(())
        }
        Cmd::Project(pc) => match pc {
            ProjectCmd::List { json } => {
                let f = snapshot(&cfg).await;
                #[derive(serde::Serialize)]
                struct Row<'a> {
                    name: &'a str,
                    repo: String,
                    worktrees_dir: String,
                    reviews: Option<&'a str>,
                    base: &'a str,
                    worktrees: usize,
                    agents: usize,
                }
                let rows: Vec<Row> = cfg
                    .projects
                    .iter()
                    .map(|p| Row {
                        name: &p.name,
                        repo: p.repo.display().to_string(),
                        worktrees_dir: p.worktrees_dir().display().to_string(),
                        reviews: p.reviews.as_deref(),
                        base: &p.base,
                        worktrees: f
                            .worktrees
                            .iter()
                            .filter(|w| w.worktree.repo == p.name)
                            .count(),
                        agents: f
                            .worktrees
                            .iter()
                            .filter(|w| w.worktree.repo == p.name)
                            .map(|w| w.agents.len())
                            .sum(),
                    })
                    .collect();
                if json {
                    return print_json(&rows);
                }
                println!(
                    "{:<18} {:<4} {:<4} {:<30} {:<8} {}",
                    "NAME", "WT", "AG", "REVIEWS", "BASE", "REPO"
                );
                for r in rows {
                    println!(
                        "{:<18} {:<4} {:<4} {:<30} {:<8} {}",
                        r.name,
                        r.worktrees,
                        r.agents,
                        r.reviews.unwrap_or("—"),
                        r.base,
                        r.repo
                    );
                }
                Ok(())
            }
            ProjectCmd::Add {
                path,
                name,
                reviews,
                worktrees,
                base,
            } => {
                let path = path.unwrap_or(std::env::current_dir()?);
                let p =
                    cmd::project::detect(&path, name, reviews, worktrees.as_deref(), base).await?;
                let cfg_path = cli_config_path(&cli_config);
                cmd::project::add_to_config(&cfg, &cfg_path, &p)?;
                println!(
                    "added project `{}` ({}, reviews {}) to {}",
                    p.name,
                    p.repo.display(),
                    p.reviews.as_deref().unwrap_or("none"),
                    cfg_path.display()
                );
                Ok(())
            }
        },
        Cmd::Setup {
            remove,
            check,
            json,
        } => {
            let engine = Engine::new(cfg.clone());
            if remove {
                let r = cmd::setup::remove()?;
                if r.is_empty() {
                    println!("nothing was installed");
                } else {
                    for x in r {
                        println!("removed {x}");
                    }
                    println!("restart running agent sessions to stop their state files");
                }
                return Ok(());
            }
            if !check {
                for h in cmd::setup::install(engine.agent_adapters())? {
                    println!("installed {:<9} {}", h.agent, h.path);
                }
                println!("restart your agent sessions to load the hooks (running ones keep the old state)\n");
                println!(
                    "tmux status bar (add to ~/.config/tmux/tmux.conf):\n{}\n",
                    cmd::setup::TMUX_SNIPPET
                );
            }
            let c = cmd::setup::check(&cfg, engine.agent_adapters()).await;
            if json {
                return print_json(&c);
            }
            for a in &c.agents {
                let hook = c.hooks.iter().find(|h| h.agent == a.agent);
                println!(
                    "{} {:<9} bin {} · {} running, {} reporting state · hooks {}{}",
                    if a.bin_found { "✓" } else { "✗" },
                    a.agent,
                    if a.bin_found { "found" } else { "NOT FOUND" },
                    a.running,
                    a.reporting,
                    match hook {
                        None => "n/a".to_string(),
                        Some(h) if !h.installed => "not installed".to_string(),
                        Some(h) if !h.up_to_date => "OUTDATED (run fleet setup)".to_string(),
                        Some(_) => "installed".to_string(),
                    },
                    hook.and_then(|h| h.note)
                        .map(|n| format!(" · {n}"))
                        .unwrap_or_default()
                );
            }
            for t in &c.tools {
                println!(
                    "{} {:<9} {}",
                    if t.ok { "✓" } else { "✗" },
                    t.name,
                    t.detail
                );
            }
            Ok(())
        }
        Cmd::AgentHook { agent } => cmd::agent_hook::run(&agent),
        Cmd::Keys => {
            for (action, key) in tui::keys::Keymap::from_config(&cfg).list() {
                println!("{action:<22} {key}");
            }
            Ok(())
        }
    }
}

/// The project whose repo contains the cwd (no single-project fallback).
fn cwd_project_strict(cfg: &Config) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cfg.project_for_path(&cwd).map(|r| r.name.clone())
}

fn cwd_repo(cfg: &Config) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cfg.project_for_path(&cwd)
        .map(|r| r.name.clone())
        .or_else(|| {
            if cfg.projects.len() == 1 {
                Some(cfg.projects[0].name.clone())
            } else {
                None
            }
        })
}

async fn agent(a: AgentCmd, cfg: Config, project: Option<String>) -> Result<()> {
    match a {
        AgentCmd::Start(args) => {
            let repo = args.repo.clone().or_else(|| cwd_repo(&cfg)).context(
                "pass --repo <name>: cwd is not inside a configured repo and config has several",
            )?;
            let prompt = match (&args.message, &args.file) {
                (Some(m), _) => m.clone(),
                (None, Some(f)) if f.as_os_str() == "-" => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
                (None, Some(f)) => std::fs::read_to_string(f)
                    .with_context(|| format!("reading {}", f.display()))?,
                (None, None) => edit_prompt(
                    &args
                        .name
                        .clone()
                        .unwrap_or_else(|| "(agent will pick)".into()),
                    &repo,
                )?,
            };
            let engine = Engine::new(cfg);
            let out = start::start(
                &engine,
                start::StartOpts {
                    name: args.name,
                    prompt,
                    repo,
                    agent: args.agent,
                    mode: args.mode,
                    model: args.model,
                    base: args.base,
                    nvim: if args.no_nvim { Some(false) } else { None },
                    attach: !args.no_attach,
                },
                &progress(),
            )
            .await?;
            println!(
                "started {} ({}) on {} at {} (tmux {}, port {})",
                out.slug,
                out.agent,
                out.branch,
                out.path.display(),
                out.tmux_session,
                out.port
            );
            Ok(())
        }
        AgentCmd::List { json } => {
            let mut f = snapshot(&cfg).await;
            if let Some(p) = &project {
                f.overview.retain(|r| r.repo() == Some(p.as_str()));
            }
            if json {
                return print_json(&f.overview);
            }
            println!(
                "{:<10} {:<26} {:<14} {:<26} {:<18} {:<20} {:<26} {}",
                "OVERALL", "NAME", "REPO", "BRANCH", "AGENT", "MODEL", "TMUX", "REVIEW"
            );
            for r in &f.overview {
                let w = r.worktree.as_ref();
                let branch = w
                    .map(|w| match &w.branch {
                        Some(b) => format!("{b} {}", w.sync_glyph()),
                        None => w.sync_glyph(),
                    })
                    .unwrap_or("—".into());
                let first = r.agents.first();
                let agent = first
                    .map(|a| {
                        format!(
                            "{} {} {}",
                            a.mode.as_deref().unwrap_or("?"),
                            a.activity.glyph(),
                            a.activity.label()
                        )
                    })
                    .unwrap_or("— NONE".into());
                let pane = r
                    .primary_pane()
                    .map(|p| format!("{} {}", p.target(), if p.attached { "●" } else { "○" }))
                    .unwrap_or("—".into());
                let review = r
                    .reviews
                    .first()
                    .map(|x| format!("{} {}", x.display_id, x.status.label()))
                    .unwrap_or("—".into());
                let model = first.and_then(|a| a.model_short()).unwrap_or("—");
                println!(
                    "{:<10} {:<26} {:<14} {:<26} {:<18} {:<20} {:<26} {}",
                    r.overall.label(),
                    trunc(&r.name(), 26),
                    trunc(r.repo().unwrap_or("—"), 14),
                    trunc(&branch, 26),
                    trunc(&agent, 18),
                    trunc(model, 20),
                    trunc(&pane, 26),
                    review
                );
                for a in r.agents.iter().skip(1) {
                    println!(
                        "{:<10}   ↳ {:<22} {:<14} {:<26} {:<18} {}",
                        "",
                        "",
                        "",
                        "",
                        format!(
                            "{} {}",
                            a.mode.as_deref().unwrap_or("?"),
                            a.activity.label()
                        ),
                        a.model_short().unwrap_or("—")
                    );
                }
            }
            for w in &f.warnings {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        AgentCmd::Attach { name } => send::attach(&snapshot(&cfg).await, &name).await,
        AgentCmd::Send {
            name,
            text,
            mode,
            model,
        } => {
            let engine = Engine::new(cfg);
            let f = engine.snapshot(false).await;
            let msg = fleet_core::agents::SendMsg {
                text: text.join(" "),
                model,
                mode,
            };
            send::send(&engine, &f, &name, &msg, &progress()).await
        }
        AgentCmd::Choices { repo, agent, json } => {
            let repo = repo
                .or(project)
                .or_else(|| cwd_repo(&cfg))
                .context("pass --repo <name>")?;
            let dir = cfg
                .project(&repo)
                .with_context(|| format!("repo `{repo}` is not in config"))?
                .repo
                .clone();
            let agent = agent.unwrap_or_else(|| cfg.defaults.agent.clone());
            let configured = cfg.agents.get(&agent).model;
            let engine = Engine::new(cfg);
            let adapter = engine
                .agent_adapter(&agent)
                .with_context(|| format!("agent `{agent}` is not enabled"))?;
            let c = adapter.choices(&dir).await?;
            if json {
                return print_json(&c);
            }
            for (i, a) in c.agents.iter().enumerate() {
                println!(
                    "{:<8} {:<12} default model: {}",
                    if i == 0 { "modes" } else { "" },
                    a.name,
                    configured
                        .as_deref()
                        .map(|m| format!("{m} (agents.{agent}.model)"))
                        .or_else(|| c.default_for(&a.name).map(String::from))
                        .unwrap_or_else(|| format!("({agent} decides)"))
                );
            }
            if c.models.is_empty() {
                println!("models   ({agent} can't list them; any name is passed through)");
            }
            for (i, m) in c.models.iter().enumerate() {
                println!("{:<8} {m}", if i == 0 { "models" } else { "" });
            }
            Ok(())
        }
        AgentCmd::Rename { old, new } => {
            let r = rename::rename(&cfg, &old, &new, &progress()).await?;
            println!(
                "renamed to {} ({}) at {}",
                r.slug,
                r.branch.as_deref().unwrap_or("detached"),
                r.path.display()
            );
            Ok(())
        }
        AgentCmd::Stop { name } => send::stop(&snapshot(&cfg).await, &name).await,
    }
}

fn edit_prompt(name: &str, repo: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("fleet-prompt-{}.md", std::process::id()));
    std::fs::write(
        &path,
        format!("\n<!-- fleet: first prompt for task `{name}` in repo `{repo}`. Lines inside this comment are removed. Save empty to abort. -->\n"),
    )?;
    let parts = shell_words::split(&editor)?;
    let st = std::process::Command::new(&parts[0])
        .args(&parts[1..])
        .arg(&path)
        .status()
        .context("running $EDITOR")?;
    if !st.success() {
        bail!("editor exited with {st}");
    }
    let s = std::fs::read_to_string(&path)?;
    std::fs::remove_file(&path).ok();
    let body = s
        .lines()
        .filter(|l| !(l.trim_start().starts_with("<!--") && l.trim_end().ends_with("-->")))
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        bail!("empty prompt, aborted");
    }
    Ok(body)
}

pub fn trunc(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
