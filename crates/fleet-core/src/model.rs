//! The frontend contract: every type here is `Serialize`, so the exact same
//! data that the TUI renders can leave the process as `fleet status --json`
//! (and later `fleet serve`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ───────────────────────── collector rows ─────────────────────────

/// One tmux pane. `pid` is the pane's shell pid (ancestor of whatever runs in it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneRow {
    pub session: String,
    pub window: u32,
    pub window_name: String,
    pub pane: u32,
    pub pid: u32,
    pub cwd: PathBuf,
    pub command: String,
    pub title: String,
    pub attached: bool,
}

impl PaneRow {
    pub fn target(&self) -> String {
        format!("{}:{}.{}", self.session, self.window, self.pane)
    }
}

/// What an agent is doing right now, as far as we can tell.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// A Plannotator server is up inside the agent process: a plan awaits you.
    WaitPlan,
    /// The agent called the `question` tool and is blocked on your answer.
    WaitQuestion,
    /// A permission prompt is pending (fleet-started agents only, via HTTP).
    WaitPermission,
    /// Last assistant message not completed: the model or a tool is running.
    Working,
    /// Last assistant message completed: waiting for your next message.
    Idle,
    /// No process.
    None,
}

impl Activity {
    /// Working, or waiting on you (plan, question, permission): stopping it cuts work off.
    /// Idle agents are not busy: `close` and gc stop them (the conversation resumes later).
    pub fn is_busy(self) -> bool {
        !matches!(self, Activity::Idle | Activity::None)
    }
    pub fn is_waiting(self) -> bool {
        matches!(
            self,
            Activity::WaitPlan | Activity::WaitQuestion | Activity::WaitPermission
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            Activity::WaitPlan => "WAIT:plan",
            Activity::WaitQuestion => "WAIT:question",
            Activity::WaitPermission => "WAIT:perm",
            Activity::Working => "WORKING",
            Activity::Idle => "IDLE",
            Activity::None => "NONE",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Activity::WaitPlan => "⏳",
            Activity::WaitQuestion => "❓",
            Activity::WaitPermission => "🔒",
            Activity::Working => "●",
            Activity::Idle => "○",
            Activity::None => "—",
        }
    }
}

/// One running agent process (opencode today).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRow {
    pub adapter: String,
    pub pid: u32,
    /// Canonicalised working directory of the process.
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub title: Option<String>,
    /// `plan` / `build` for opencode.
    pub mode: Option<String>,
    /// `provider/model` the session last used.
    #[serde(default)]
    pub model: Option<String>,
    pub activity: Activity,
    /// The tool currently running, when `activity == Working`.
    pub running_tool: Option<String>,
    pub plan_url: Option<String>,
    pub plan_title: Option<String>,
    /// HTTP port when started by fleet (`--port`).
    pub port: Option<u16>,
    pub last_activity: Option<DateTime<Utc>>,
    /// tmux target (`sess:w.p`) of the pane hosting this process, filled by the joiner.
    #[serde(default)]
    pub pane_target: Option<String>,
    /// Pending permission prompts (titles), from the fleet opencode plugin.
    #[serde(default)]
    pub pending: Vec<String>,
    /// The fleet plugin's control endpoint in this process (messages, model switches).
    #[serde(default)]
    pub control_url: Option<String>,
    /// Bearer token for `control_url`. Never serialised (`fleet status --json` is public).
    #[serde(skip)]
    pub control_token: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Ci {
    Passed,
    Failed,
    Running,
    None,
}

impl Ci {
    pub fn glyph(self) -> &'static str {
        match self {
            Ci::Passed => "✓",
            Ci::Failed => "✗",
            Ci::Running => "⟳",
            Ci::None => "–",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Draft,
    Open,
    Approved,
    Mergeable,
    Conflicts,
    CiFailed,
    Merged,
    Closed,
}

impl ReviewStatus {
    pub fn label(self) -> &'static str {
        match self {
            ReviewStatus::Draft => "draft",
            ReviewStatus::Open => "open",
            ReviewStatus::Approved => "approved",
            ReviewStatus::Mergeable => "mergeable",
            ReviewStatus::Conflicts => "conflicts",
            ReviewStatus::CiFailed => "ci failed",
            ReviewStatus::Merged => "merged",
            ReviewStatus::Closed => "closed",
        }
    }
    pub fn is_done(self) -> bool {
        matches!(self, ReviewStatus::Merged | ReviewStatus::Closed)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Threads {
    pub total: u32,
    pub resolved: u32,
    pub unresolved: u32,
}

/// One review (MR on GitLab, PR on GitHub).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRow {
    /// `gitlab:group/project`
    pub source: String,
    /// The forge's own id (iid on GitLab).
    pub id: u64,
    /// `!514` or `#12`
    pub display_id: String,
    pub title: String,
    pub author: String,
    pub source_branch: String,
    pub target_branch: String,
    pub draft: bool,
    pub ci: Ci,
    pub approvals: Vec<String>,
    pub threads: Threads,
    pub status: ReviewStatus,
    pub head_sha: String,
    pub url: String,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Sync {
    Synced,
    Ahead,
    Behind,
    Diverged,
    NoUpstream,
    Detached,
    Missing,
}

/// One git worktree (the main clone included).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeRow {
    pub repo: String,
    pub path: PathBuf,
    /// Folder name.
    pub name: String,
    pub branch: Option<String>,
    pub head: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
    pub detached: bool,
    pub is_main: bool,
    pub missing: bool,
    /// The branch tip is in the project's base branch (local or remote-tracking): merged with
    /// a merge commit, fast-forwarded, or no commits of its own yet.
    #[serde(default)]
    pub in_base: bool,
}

impl WorktreeRow {
    pub fn sync(&self) -> Sync {
        if self.missing {
            Sync::Missing
        } else if self.detached {
            Sync::Detached
        } else if self.upstream.is_none() {
            Sync::NoUpstream
        } else {
            match (self.ahead, self.behind) {
                (0, 0) => Sync::Synced,
                (_, 0) => Sync::Ahead,
                (0, _) => Sync::Behind,
                _ => Sync::Diverged,
            }
        }
    }
    pub fn sync_glyph(&self) -> String {
        match self.sync() {
            Sync::Synced => "=".into(),
            Sync::Ahead => format!("↑{}", self.ahead),
            Sync::Behind => format!("↓{}", self.behind),
            Sync::Diverged => format!("↕{}/{}", self.ahead, self.behind),
            Sync::NoUpstream => "⊘".into(),
            Sync::Detached => "(detached)".into(),
            Sync::Missing => "✗".into(),
        }
    }
}

// ───────────────────────── joined rows ─────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    NeedsYou,
    Failed,
    Ready,
    Merged,
    Working,
    Idle,
    Dead,
}

impl Overall {
    pub fn label(self) -> &'static str {
        match self {
            Overall::NeedsYou => "NEEDS YOU",
            Overall::Failed => "FAILED",
            Overall::Ready => "READY",
            Overall::Merged => "MERGED",
            Overall::Working => "WORKING",
            Overall::Idle => "IDLE",
            Overall::Dead => "DEAD",
        }
    }
}

/// One Overview row: a worktree with everything joined onto it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewRow {
    pub overall: Overall,
    /// `None` for the "outside repos" group (an agent in a directory that is no worktree).
    pub worktree: Option<WorktreeRow>,
    /// Directory shown when there is no worktree.
    pub dir: PathBuf,
    pub agents: Vec<AgentRow>,
    pub panes: Vec<PaneRow>,
    pub reviews: Vec<ReviewRow>,
    pub last_activity: Option<DateTime<Utc>>,
}

impl OverviewRow {
    pub fn name(&self) -> String {
        match &self.worktree {
            Some(w) => w.name.clone(),
            None => self.dir.to_string_lossy().into_owned(),
        }
    }
    pub fn repo(&self) -> Option<&str> {
        self.worktree.as_ref().map(|w| w.repo.as_str())
    }
    /// The pane that hosts the first agent, else the first pane in the dir.
    pub fn primary_pane(&self) -> Option<&PaneRow> {
        self.agents
            .first()
            .and_then(|a| {
                self.panes
                    .iter()
                    .find(|p| p.pid == a.pid || p.command == a.adapter)
            })
            .or_else(|| self.panes.first())
    }
}

/// Joined pane: the pane plus what we know runs inside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneView {
    pub pane: PaneRow,
    pub worktree: Option<WorktreeRow>,
    pub agent: Option<AgentRow>,
}

/// Joined review: the review plus local worktrees/agents on its branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewView {
    pub review: ReviewRow,
    pub worktrees: Vec<WorktreeRow>,
    pub agents: Vec<AgentRow>,
}

/// Joined worktree: the worktree plus its agents and review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeView {
    pub worktree: WorktreeRow,
    pub overall: Overall,
    pub agents: Vec<AgentRow>,
    pub panes: Vec<PaneRow>,
    pub review: Option<ReviewRow>,
    pub gc: GcVerdict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "verdict", content = "reason")]
pub enum GcVerdict {
    Yes(String),
    No(String),
}

impl GcVerdict {
    pub fn label(&self) -> String {
        match self {
            GcVerdict::Yes(r) => format!("yes: {r}"),
            GcVerdict::No(r) => format!("no: {r}"),
        }
    }
}

/// The whole snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Fleet {
    pub taken_at: Option<DateTime<Utc>>,
    pub reviews_fetched_at: Option<DateTime<Utc>>,
    pub overview: Vec<OverviewRow>,
    pub panes: Vec<PaneView>,
    pub worktrees: Vec<WorktreeView>,
    pub reviews: Vec<ReviewView>,
    /// Non-fatal collector problems, shown in the status line.
    pub warnings: Vec<String>,
}

impl Fleet {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in &self.overview {
            match r.overall {
                Overall::NeedsYou => c.needs_you += 1,
                Overall::Failed => c.failed += 1,
                Overall::Ready => c.ready += 1,
                _ => {}
            }
        }
        c.agents = self.overview.iter().map(|r| r.agents.len()).sum();
        c.worktrees = self.worktrees.len();
        c.reviews = self.reviews.len();
        c
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub agents: usize,
    pub worktrees: usize,
    pub reviews: usize,
    pub needs_you: usize,
    pub failed: usize,
    pub ready: usize,
}

impl AgentRow {
    /// The model without its provider (`anthropic/claude-opus-5-5` -> `claude-opus-5-5`),
    /// for narrow columns.
    pub fn model_short(&self) -> Option<&str> {
        self.model
            .as_deref()
            .map(|m| m.split_once('/').map(|(_, n)| n).unwrap_or(m))
    }
}
