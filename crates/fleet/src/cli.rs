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
    /// Config file (default: ~/.config/fleet/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
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
        /// Write the example config if none exists
        #[arg(long)]
        init: bool,
    },
    /// Print every action and its current key binding
    Keys,
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
    /// Send a message to a running agent
    Send { name: String, text: Vec<String> },
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
    /// opencode agent: plan | build
    #[arg(long)]
    mode: Option<String>,
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
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(dispatch(cli, cfg))
}

async fn snapshot(cfg: &Config) -> Fleet {
    Engine::new(cfg.clone()).snapshot(true).await
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

async fn dispatch(cli: Cli, cfg: Config) -> Result<()> {
    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Tui => {
            if cfg.repos.is_empty() {
                eprintln!(
                    "fleet: no repos configured. Run `fleet config --init` and edit {}",
                    fleet_core::config::config_path().display()
                );
            }
            tui::run(cfg).await
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
        Cmd::Agent(a) => agent(a, cfg).await,
        Cmd::Start(args) => agent(AgentCmd::Start(args), cfg).await,
        Cmd::Review(r) => match r {
            ReviewCmd::List { json, source } => {
                let f = snapshot(&cfg).await;
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
                no_attach,
            } => {
                let engine = Engine::new(cfg);
                let repo = repo.or_else(|| cwd_repo(&engine.config));
                let out = review::start(
                    &engine,
                    review::ReviewOpts {
                        id_or_url,
                        repo,
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
                let f = snapshot(&cfg).await;
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
            if init {
                if p.exists() {
                    bail!("{} already exists", p.display());
                }
                std::fs::create_dir_all(p.parent().unwrap())?;
                std::fs::write(&p, Config::example())?;
                println!("wrote example config");
            } else if !p.exists() {
                println!("(not found; `fleet config --init` writes an example)\n");
                print!("{}", Config::example());
            }
            Ok(())
        }
        Cmd::Keys => {
            for (action, key) in tui::keys::Keymap::from_config(&cfg).list() {
                println!("{action:<22} {key}");
            }
            Ok(())
        }
    }
}

fn cwd_repo(cfg: &Config) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cfg.repo_for_path(&cwd).map(|r| r.name.clone()).or_else(|| {
        if cfg.repos.len() == 1 {
            Some(cfg.repos[0].name.clone())
        } else {
            None
        }
    })
}

async fn agent(a: AgentCmd, cfg: Config) -> Result<()> {
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
                    mode: args.mode,
                    base: args.base,
                    nvim: if args.no_nvim { Some(false) } else { None },
                    attach: !args.no_attach,
                },
                &progress(),
            )
            .await?;
            println!(
                "started {} on {} at {} (tmux {}, port {})",
                out.slug,
                out.branch,
                out.path.display(),
                out.tmux_session,
                out.port
            );
            Ok(())
        }
        AgentCmd::List { json } => {
            let f = snapshot(&cfg).await;
            if json {
                return print_json(&f.overview);
            }
            println!(
                "{:<10} {:<26} {:<14} {:<26} {:<18} {:<26} {}",
                "OVERALL", "NAME", "REPO", "BRANCH", "AGENT", "TMUX", "REVIEW"
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
                println!(
                    "{:<10} {:<26} {:<14} {:<26} {:<18} {:<26} {}",
                    r.overall.label(),
                    trunc(&r.name(), 26),
                    trunc(r.repo().unwrap_or("—"), 14),
                    trunc(&branch, 26),
                    trunc(&agent, 18),
                    trunc(&pane, 26),
                    review
                );
                for a in r.agents.iter().skip(1) {
                    println!(
                        "{:<10}   ↳ {:<22} {:<14} {:<26} {} {}",
                        "",
                        "",
                        "",
                        "",
                        a.mode.as_deref().unwrap_or("?"),
                        a.activity.label()
                    );
                }
            }
            for w in &f.warnings {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        AgentCmd::Attach { name } => send::attach(&snapshot(&cfg).await, &name).await,
        AgentCmd::Send { name, text } => {
            let engine = Engine::new(cfg);
            let f = engine.snapshot(false).await;
            send::send(&engine, &f, &name, &text.join(" ")).await
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
