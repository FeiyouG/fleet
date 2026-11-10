use crate::tui;
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use fleet_core::cmd::{self, close, gc, rename, review, send, start};
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
    /// Worktrees: list, gc, close
    #[command(subcommand)]
    Worktree(WorktreeCmd),
    /// tmux: list sessions/windows/panes with what runs in them
    #[command(subcommand)]
    Tmux(TmuxCmd),
    /// Machines (laptop, cloud): list, pause, resume, keep awake, shell, …
    #[command(subcommand)]
    Machine(MachineCmd),
    /// Credentials for cloud machines
    #[command(subcommand)]
    Creds(CredsCmd),
    /// Alias of `agent start`
    Start(StartArgs),
    /// Alias of `worktree close`
    Close(CloseArgs),
    /// Start a task preset from config: `fleet task bugfix -m "…"`, `fleet task review 514`
    Task(TaskArgs),
    /// Print the config path and an example config
    Config {
        /// Write the example config if none exists (the cwd's repo becomes the first project)
        #[arg(long)]
        init: bool,
        /// Print the shipped default configuration (every file)
        #[arg(long)]
        default: bool,
        /// Print the effective configuration (defaults + your file, merged) as JSON
        #[arg(long)]
        effective: bool,
        /// Validate the configuration: list fatal problems and warnings, exit 1 on fatal
        #[arg(long)]
        check: bool,
    },
    /// Projects: list, add
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Print every action and its current key binding (per project with --project)
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
    /// Open nvim's diff of the task against its base in a new window of its tmux session
    /// (on its machine), then attach
    Diff { name: String },
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
    /// Task preset from config (default: start). `fleet task NAME …` is the same thing
    #[arg(long)]
    task: Option<String>,
    /// Which agent (a name from `agents`; default: the task's, else defaults.agent)
    #[arg(long)]
    agent: Option<String>,
    /// plan | build | yolo, or a harness mode (`fleet agent choices` lists them)
    #[arg(long)]
    mode: Option<String>,
    /// Skill files (absolute SKILL.md paths) to load instead of the task's; repeatable
    #[arg(long = "skill", value_name = "PATH")]
    skills: Vec<PathBuf>,
    /// Load no skills even when the task or agent lists some
    #[arg(long, conflicts_with = "skills")]
    no_skills: bool,
    /// Model in the agent's naming, e.g. opencode provider/model (default: agents.<name>.model)
    #[arg(long)]
    model: Option<String>,
    /// Base branch (default: repo's `base`, usually main)
    #[arg(long)]
    base: Option<String>,
    /// Command for the left (shell) pane, e.g. `nvim`; overrides defaults.left_pane
    #[arg(long, value_name = "CMD")]
    left_pane: Option<String>,
    /// Leave the left pane a bare shell even when defaults.left_pane is set
    #[arg(long, conflicts_with = "left_pane")]
    no_left_pane: bool,
    /// Machine profile to run on (a name from `machines`; default: the task's, else
    /// defaults.machine)
    #[arg(long)]
    machine: Option<String>,
    /// Do not attach after starting
    #[arg(long)]
    no_attach: bool,
}

#[derive(Args)]
struct TaskArgs {
    /// Task name from config (`fleet config --effective` lists them)
    task: String,
    /// For `on = "review"` tasks: the MR/PR id or URL. For project tasks: the task slug.
    target: Option<String>,
    /// Prompt text (project tasks; else -f FILE, else the task's preset / $EDITOR)
    #[arg(short = 'm', long)]
    message: Option<String>,
    #[arg(short = 'f', long)]
    file: Option<PathBuf>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    mode: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long = "skill", value_name = "PATH")]
    skills: Vec<PathBuf>,
    #[arg(long, conflicts_with = "skills")]
    no_skills: bool,
    /// Machine profile to run on (default: the task's, else defaults.machine)
    #[arg(long)]
    machine: Option<String>,
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
        /// Task preset from config, an `on = "review"` one (default: review)
        #[arg(long)]
        task: Option<String>,
        /// Which agent (default: the task's, else defaults.agent)
        #[arg(long)]
        agent: Option<String>,
        /// Skill files to load instead of the task's; repeatable
        #[arg(long = "skill", value_name = "PATH")]
        skills: Vec<PathBuf>,
        #[arg(long, conflicts_with = "skills")]
        no_skills: bool,
        /// Machine profile to run on (default: the task's, else defaults.machine)
        #[arg(long)]
        machine: Option<String>,
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
    /// Finish a task: stop its agents, kill its tmux, remove the worktree and its local
    /// branch, close the registry task. All-or-nothing; refuses when files are uncommitted,
    /// the branch is not merged, an agent is busy, or the worktree is locked.
    Close(CloseArgs),
}

#[derive(Args, Clone)]
struct CloseArgs {
    /// Task name, worktree folder name, branch, tmux session, or a path
    target: String,
    /// Close even with uncommitted files, an unmerged branch, a busy agent or a lock
    #[arg(long)]
    force: bool,
    /// Do not ask (without it, a non-interactive run is a dry run)
    #[arg(long, short = 'y')]
    yes: bool,
    /// Print the plan (and the result) as JSON
    #[arg(long)]
    json: bool,
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
    /// Bring a session/window/pane in front of you from outside tmux (notification clicks use
    /// this): switches every attached client and activates the terminal app
    Focus {
        /// `session`, `session:win` or `session:win.pane`
        target: String,
    },
}

#[derive(Subcommand)]
enum MachineCmd {
    /// List the machines of every machine profile (never wakes a paused machine)
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Create a machine from a profile (task starts do this for you)
    Create {
        profile: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Pause: memory kept, compute billing stops
    Pause { machine: String },
    /// Resume (or wake) a paused machine
    Resume { machine: String },
    /// Stop: processes end, the disk is kept
    Stop { machine: String },
    /// Start a stopped machine
    Start { machine: String },
    /// Delete a machine and everything on it
    Rm {
        machine: String,
        /// Do not ask (required without a terminal)
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Keep a machine awake for a while (e.g. a silent background job)
    Keep {
        machine: String,
        /// e.g. 30m, 1h (InstaBox renews at most 1h at a time)
        #[arg(default_value = "1h")]
        duration: String,
    },
    /// Release a `keep`
    Release { machine: String },
    /// Pause after this much idle time (e.g. 10m), or `off`
    Idle { machine: String, duration: String },
    /// Pause after this much continuous running (e.g. 4h), or `off`
    MaxRun { machine: String, duration: String },
    /// Open a shell on a machine (wakes it), or run a command there with a terminal
    Shell {
        machine: String,
        /// Print the command instead of running it (for scripts and other terminals)
        #[arg(long)]
        print: bool,
        /// Command to run instead of a login shell
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Check a machine has tmux, git and the harness
    Doctor {
        machine: String,
        #[arg(long, default_value = "opencode")]
        harness: String,
    },
    /// A private link to a port on a machine (e.g. the plan review page)
    Url { machine: String, port: u16 },
    /// Build a reusable snapshot for a profile: tmux, fleet, the project's clone, your
    /// opencode config (no logins), `fleet setup`, the profile's `setup` commands
    Snapshot {
        profile: String,
        /// Project to clone into the snapshot (default: the cwd's project)
        #[arg(long)]
        project: Option<String>,
        /// Snapshot name (default: fleet-<profile>-<project>)
        #[arg(long)]
        name: Option<String>,
    },
    /// Inside a machine: publish its agents' state to the provider (started by fleet)
    #[command(hide = true)]
    Publish {
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand)]
enum CredsCmd {
    /// Push each cloud profile's `files` (login files) as provider secrets, refresh tokens
    /// removed; unchanged files are skipped
    Sync {
        /// Only this machine profile
        #[arg(long)]
        profile: Option<String>,
        /// Keep running, syncing every N seconds
        #[arg(long, value_name = "SECS")]
        watch: Option<u64>,
    },
}

async fn machine(m: MachineCmd, cfg: Config, project: Option<String>) -> Result<()> {
    use fleet_core::cmd::machine as mc;
    use fleet_core::machines::remote;
    let one = |s: &str| -> Vec<String> { vec![s.to_string()] };
    match m {
        MachineCmd::Ls { json } => {
            let (rows, warnings) = mc::list(&cfg).await;
            if json {
                print_json(&rows)?;
            } else {
                println!(
                    "{:<24} {:<9} {:<10} {:<9} {:<5} {:<6} {:<7} TASKS",
                    "MACHINE", "PROVIDER", "PROFILE", "STATE", "SIZE", "IDLE", "MAXRUN"
                );
                for l in &rows {
                    let r = &l.row;
                    println!(
                        "{:<24} {:<9} {:<10} {:<9} {:<5} {:<6} {:<7} {}",
                        r.name,
                        r.provider,
                        l.profile,
                        r.state.label(),
                        r.size.map(|s| s.label()).unwrap_or_else(|| "—".into()),
                        mc::fmt_duration(r.idle_after),
                        mc::fmt_duration(r.max_run),
                        l.tasks.join(",")
                    );
                }
            }
            for w in warnings {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        MachineCmd::Create { profile, name } => {
            let r = mc::create(&cfg, &profile, name.as_deref()).await?;
            println!("created {} ({}, {})", r.name, r.id, r.state.label());
            Ok(())
        }
        MachineCmd::Pause { machine } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            a.pause(&r.id).await?;
            println!("paused {}", r.name);
            Ok(())
        }
        MachineCmd::Resume { machine } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            a.resume(&r.id).await?;
            println!("resumed {}", r.name);
            Ok(())
        }
        MachineCmd::Stop { machine } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            a.stop(&r.id).await?;
            println!("stopped {}", r.name);
            Ok(())
        }
        MachineCmd::Start { machine } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            a.start(&r.id).await?;
            println!("started {}", r.name);
            Ok(())
        }
        MachineCmd::Rm { machine, yes } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            let tasks: Vec<String> = fleet_core::registry::Registry::open()
                .and_then(|reg| reg.open_tasks())
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.machine_id == r.id)
                .map(|t| t.slug)
                .collect();
            if !tasks.is_empty() {
                eprintln!(
                    "{} hosts open tasks: {} (their work is lost unless pushed)",
                    r.name,
                    tasks.join(", ")
                );
            }
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("refusing to delete {} without --yes", r.name);
                }
                eprint!("delete {} and everything on it? [y/N] ", r.name);
                std::io::stderr().flush().ok();
                let mut s = String::new();
                std::io::stdin().read_line(&mut s)?;
                if !matches!(s.trim(), "y" | "Y" | "yes") {
                    bail!("not deleted");
                }
            }
            a.destroy(&r.id).await?;
            if let Ok(reg) = fleet_core::registry::Registry::open() {
                for t in &tasks {
                    reg.close(t).ok();
                }
            }
            println!("deleted {}", r.name);
            Ok(())
        }
        MachineCmd::Keep { machine, duration } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            let d = mc::parse_duration(&duration)?.context("keep: give a duration, e.g. 1h")?;
            a.keepalive(&r.id, "fleet-keep", d).await?;
            println!("keeping {} awake for {}", r.name, mc::fmt_duration(Some(d)));
            Ok(())
        }
        MachineCmd::Release { machine } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            a.release(&r.id, "fleet-keep").await?;
            println!("released {}", r.name);
            Ok(())
        }
        MachineCmd::Idle { machine, duration } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            let d = mc::parse_duration(&duration)?;
            a.set_idle(&r.id, d).await?;
            println!("{}: pauses after {} idle", r.name, mc::fmt_duration(d));
            Ok(())
        }
        MachineCmd::MaxRun { machine, duration } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            let d = mc::parse_duration(&duration)?;
            a.set_max_run(&r.id, d).await?;
            println!("{}: max run {}", r.name, mc::fmt_duration(d));
            Ok(())
        }
        MachineCmd::Shell {
            machine,
            print,
            cmd,
        } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            let run = if !cmd.is_empty() {
                cmd
            } else if r.provider == "local" {
                one(&std::env::var("SHELL").unwrap_or_else(|_| "sh".into()))
            } else {
                vec![]
            };
            let tty = a.tty_command(&r.id, &run).await?;
            if print {
                println!("{}", shell_words::join(&tty));
                return Ok(());
            }
            remote::open_tty(&tty, &r.name).await
        }
        MachineCmd::Doctor { machine, harness } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            let on = remote::On {
                adapter: a.as_ref(),
                id: r.id.clone(),
            };
            let checks = remote::doctor(&on, &harness).await?;
            let mut missing = vec![];
            for (tool, ok) in &checks {
                println!("{} {tool}", if *ok { "✓" } else { "✕" });
                if !ok {
                    missing.push(tool.clone());
                }
            }
            if !missing.is_empty() {
                bail!("{} is missing {}", r.name, missing.join(", "));
            }
            Ok(())
        }
        MachineCmd::Snapshot {
            profile,
            project: p,
            name,
        } => {
            let p = p
                .or(project)
                .or_else(|| cwd_repo(&cfg))
                .context("pass --project <name>")?;
            let name = name.unwrap_or_else(|| format!("fleet-{profile}-{p}"));
            let id = mc::build_snapshot(&cfg, &profile, &p, &name, &progress()).await?;
            println!("snapshot {name} ({id}) is ready; use it with machines.{profile}.snapshot = \"{name}\"");
            Ok(())
        }
        MachineCmd::Publish { once } => mc::publish_loop(once).await,
        MachineCmd::Url { machine, port } => {
            let (_, a, r) = mc::find(&cfg, &machine).await?;
            println!("{}", a.port_url(&r.id, port).await?);
            Ok(())
        }
    }
}

/// `--skill PATH…` / `--no-skills` -> the task's skills override.
fn skills_opt(skills: Vec<PathBuf>, none: bool) -> Option<Vec<PathBuf>> {
    if none {
        Some(vec![])
    } else if skills.is_empty() {
        None
    } else {
        Some(skills)
    }
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

/// A one-shot snapshot without per-branch merged-review lookups: one forge call per project,
/// not one per worktree (`fleet status` may run every few seconds from a status bar).
async fn snapshot(cfg: &Config) -> Fleet {
    Engine::new(cfg.clone())
        .merged_lookups(false)
        .snapshot(true)
        .await
}

/// A snapshot with merged reviews looked up per worktree branch, for the commands that show
/// or act on gc verdicts (`worktree list`, `worktree gc`).
async fn snapshot_gc(engine: &Engine) -> Fleet {
    engine.snapshot(true).await
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
        Cmd::Task(t) => {
            let repo_name = t
                .repo
                .clone()
                .or(project.clone())
                .or_else(|| cwd_repo(&cfg));
            let proj = repo_name.as_deref().and_then(|r| cfg.project(r));
            let resolved = cfg.task(&t.task, proj)?;
            match resolved.on {
                fleet_core::config::On::Review => {
                    let id = t.target.clone().context(
                        "this task runs on a review: pass the MR/PR id or URL, e.g. `fleet task review 514`",
                    )?;
                    let engine = Engine::new(cfg).merged_lookups(false);
                    let out = review::start(
                        &engine,
                        review::ReviewOpts {
                            id_or_url: id,
                            task: Some(t.task),
                            repo: repo_name,
                            agent: t.agent,
                            skills: skills_opt(t.skills, t.no_skills),
                            machine: t.machine,
                            attach: !t.no_attach,
                        },
                        &progress(),
                    )
                    .await?;
                    if let review::ReviewOutcome::Started { slug, path, port } = out {
                        println!("started {slug} at {} (port {port})", path.display());
                    }
                    Ok(())
                }
                fleet_core::config::On::Project => {
                    let args = StartArgs {
                        name: t.target,
                        message: t.message.or(resolved.prompt.clone()),
                        file: t.file,
                        repo: repo_name,
                        task: Some(t.task),
                        agent: t.agent,
                        mode: t.mode,
                        model: t.model,
                        skills: t.skills,
                        no_skills: t.no_skills,
                        base: None,
                        left_pane: None,
                        no_left_pane: false,
                        machine: t.machine,
                        no_attach: t.no_attach,
                    };
                    agent(AgentCmd::Start(args), cfg, project).await
                }
            }
        }
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
                task,
                agent,
                skills,
                no_skills,
                machine,
                no_attach,
            } => {
                let engine = Engine::new(cfg).merged_lookups(false);
                let repo = repo.or(project).or_else(|| cwd_repo(&engine.config));
                let out = review::start(
                    &engine,
                    review::ReviewOpts {
                        id_or_url,
                        task,
                        repo,
                        agent,
                        skills: skills_opt(skills, no_skills),
                        machine,
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
                let mut f = snapshot_gc(&Engine::new(cfg.clone())).await;
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
                let engine = Engine::new(cfg.clone());
                let f = snapshot_gc(&engine).await;
                let plan = gc::plan(&f, &cfg).await;
                if json {
                    print_json(&plan)?;
                    if !yes {
                        return Ok(());
                    }
                } else {
                    if plan.txns.is_empty() {
                        println!("nothing to clean");
                    }
                    for t in &plan.txns {
                        println!("  {}", gc::describe(t));
                    }
                    for (n, r) in &plan.blocked {
                        println!("  blocked: {n} ({r})");
                    }
                }
                if plan.txns.is_empty() {
                    return Ok(());
                }
                if !yes && !confirm(&format!("apply {} items?", plan.txns.len()))? {
                    return Ok(());
                }
                let fresh = snapshot_gc(&engine).await;
                let reports = gc::apply(&plan, &fresh, &cfg, &progress()).await;
                let failed: Vec<_> = reports.iter().filter(|r| !r.ok()).collect();
                for r in &failed {
                    eprintln!("{}:", r.label);
                    for l in r.failure_lines() {
                        eprintln!("  {l}");
                    }
                }
                if !failed.is_empty() {
                    bail!(
                        "{} of {} item(s) not done (failed and rolled back, or skipped)",
                        failed.len(),
                        reports.len()
                    );
                }
                Ok(())
            }
            WorktreeCmd::Close(args) => close_cmd(args, cfg, project).await,
        },
        Cmd::Close(args) => close_cmd(args, cfg, project).await,
        Cmd::Machine(m) => machine(m, cfg, project).await,
        Cmd::Creds(CredsCmd::Sync { profile, watch }) => loop {
            for (secret, pushed) in
                fleet_core::cmd::machine::creds_sync(&cfg, profile.as_deref()).await?
            {
                if pushed || watch.is_none() {
                    println!("{secret}: {}", if pushed { "pushed" } else { "unchanged" });
                }
            }
            match watch {
                Some(s) => tokio::time::sleep(std::time::Duration::from_secs(s.max(30))).await,
                None => return Ok(()),
            }
        },
        Cmd::Tmux(TmuxCmd::Focus { target }) => fleet_core::tmux::focus(&target).await,
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
        Cmd::Config {
            init,
            default,
            effective,
            check,
        } => {
            let p = fleet_core::config::config_path();
            if default {
                for (path, contents) in fleet_core::config::shipped::FILES {
                    println!("-- ===== config/default/{path} =====\n{contents}");
                }
                return Ok(());
            }
            if effective {
                let user = p.exists().then(|| {
                    (
                        fleet_core::config::lua::Tree::Dir(p.parent().unwrap().to_path_buf()),
                        "init.lua",
                    )
                });
                let v = Config::effective_json(user.as_ref().map(|(t, e)| (t, *e)))?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            if check {
                // `cfg` loaded above, so there was no fatal problem; show the warnings.
                if cfg.warnings.is_empty() {
                    println!("ok: {} (no warnings)", p.display());
                } else {
                    println!("ok with warnings: {}", p.display());
                    for w in &cfg.warnings {
                        println!("  ⚠ {w}");
                    }
                }
                return Ok(());
            }
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
            let proj = project.as_deref().and_then(|p| cfg.project(p));
            let km = tui::keys::Keymap::from_config(&cfg, proj);
            for e in &km.overrides_errors {
                eprintln!("fleet: {e}");
            }
            for (action, key) in km.list() {
                println!("{action:<22} {key}");
            }
            Ok(())
        }
    }
}

/// `[y/N]` on a terminal; without one it is a dry run (returns false).
fn confirm(q: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        println!("(dry run; pass --yes to apply)");
        return Ok(false);
    }
    print!("{q} [y/N] ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    if matches!(s.trim(), "y" | "Y" | "yes") {
        Ok(true)
    } else {
        println!("aborted");
        Ok(false)
    }
}

async fn close_cmd(a: CloseArgs, cfg: Config, project: Option<String>) -> Result<()> {
    // A task on a cloud machine closes through its machine.
    if let Some(t) = fleet_core::cmd::remote_task::find(&a.target)? {
        let r = fleet_core::cmd::remote_task::reach(&cfg, t)?;
        let what = if r.profile.per_task {
            format!(
                "delete machine {} with task {}",
                r.task.machine_id, r.task.slug
            )
        } else {
            format!(
                "remove task {} (session, worktree, branch) from machine {}",
                r.task.slug, r.task.machine_id
            )
        };
        if !a.yes {
            if !std::io::stdin().is_terminal() {
                println!("would {what} (dry run; --yes to do it)");
                return Ok(());
            }
            eprint!("{what}? [y/N] ");
            std::io::stderr().flush().ok();
            let mut s = String::new();
            std::io::stdin().read_line(&mut s)?;
            if !matches!(s.trim(), "y" | "Y" | "yes") {
                bail!("not closed");
            }
        }
        fleet_core::cmd::remote_task::close(&r, a.force, &progress()).await?;
        println!("closed {}", r.task.slug);
        return Ok(());
    }
    // close asks the forge about its one branch itself; no per-worktree lookups needed.
    let engine = Engine::new(cfg.clone()).merged_lookups(false);
    let f = engine.snapshot(true).await;
    let r = close::resolve(&f, &cfg, &a.target, project.as_deref()).await?;
    let opts = close::CloseOpts { force: a.force };
    let plan = close::plan(&close::gather(&r, &f).await?, &opts)?;
    if a.json {
        print_json(&plan)?;
    } else {
        println!("{}", plan.header());
        let w = plan
            .step_lines()
            .iter()
            .map(|(d, _)| d.chars().count())
            .max()
            .unwrap_or(0);
        for (i, (d, u)) in plan.step_lines().iter().enumerate() {
            println!("  {} {d:<w$}  {u}", i + 1);
        }
        for n in &plan.notes {
            println!("  note: {n}");
        }
        if plan.force && !plan.blockers.is_empty() {
            println!("--force: these will be lost:");
            for l in plan.blocker_lines(10) {
                println!("  {l}");
            }
        }
    }
    if plan.blocked() {
        bail!("{}", close::refusal(&plan));
    }
    if !a.yes && !confirm(&format!("close {}?", plan.target.name))? {
        return Ok(());
    }
    let fresh = engine.snapshot(false).await;
    let rep = close::apply_real(&plan, &fresh, &opts, &progress()).await?;
    if a.json {
        print_json(&rep)?;
    }
    if !rep.ok() {
        for l in rep.failure_lines() {
            eprintln!("{l}");
        }
        bail!("close {} failed", plan.target.name);
    }
    if !a.json {
        println!("closed {}", plan.target.name);
        for s in &rep.done {
            if let cmd::txn::Step::StopAgent {
                adapter,
                session_id,
                ..
            } = s
            {
                if let Some(h) = cmd::txn::resume_hint(adapter, session_id.as_deref()) {
                    println!("  resume its conversation later: {h}");
                }
            }
        }
    }
    Ok(())
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
            let engine = Engine::new(cfg).merged_lookups(false);
            let out = start::start(
                &engine,
                start::StartOpts {
                    task: args.task,
                    name: args.name,
                    prompt,
                    repo,
                    agent: args.agent,
                    mode: args.mode,
                    model: args.model,
                    skills: skills_opt(args.skills, args.no_skills),
                    base: args.base,
                    left_pane: if args.no_left_pane {
                        Some(None)
                    } else {
                        args.left_pane.map(Some)
                    },
                    machine: args.machine,
                    attach: !args.no_attach,
                },
                &progress(),
            )
            .await?;
            println!(
                "started {} ({}) on {} at {} on {} (tmux {}, port {})",
                out.slug,
                out.agent,
                out.branch,
                out.path.display(),
                out.machine,
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
        AgentCmd::Attach { name } => send::attach(&cfg, &snapshot(&cfg).await, &name).await,
        AgentCmd::Send {
            name,
            text,
            mode,
            model,
        } => {
            let engine = Engine::new(cfg).merged_lookups(false);
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
            let configured = cfg.agent(&agent, None).and_then(|a| a.model);
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
        AgentCmd::Diff { name } => send::diff(&cfg, &name).await,
        AgentCmd::Stop { name } => send::stop(&cfg, &snapshot(&cfg).await, &name).await,
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
