//! Run every collector, join, return a `Fleet`. Reviews are cached for `review_ttl_secs`
//! and refreshed in the background so the UI never waits on the forge.

use crate::agents::{self, AgentAdapter, DiscoverCtx};
use crate::config::Config;
use crate::join::{self, Inputs};
use crate::model::{Fleet, ReviewRow, WorktreeRow};
use crate::procs::ProcTable;
use crate::registry::Registry;
use crate::reviews::{self, ReviewAdapter};
use crate::tmux;
use crate::worktrees::git;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

struct ReviewCache {
    rows: Vec<ReviewRow>,
    fetched_at: Option<DateTime<Utc>>,
    fetched_instant: Option<Instant>,
    in_flight: bool,
    last_error: Option<String>,
}

pub struct Engine {
    pub config: Config,
    agent_adapters: Vec<Box<dyn AgentAdapter>>,
    review_adapters: Vec<Box<dyn ReviewAdapter>>,
    reviews: Arc<Mutex<ReviewCache>>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        let agent_adapters = agents::adapters(&config.agents.adapters);
        let mut sources: Vec<String> = config
            .repos
            .iter()
            .filter_map(|r| r.review_source.clone())
            .collect();
        sources.sort();
        sources.dedup();
        let review_adapters = sources
            .iter()
            .filter_map(|s| reviews::adapter_for(s).ok())
            .collect();
        Self {
            config,
            agent_adapters,
            review_adapters,
            reviews: Arc::new(Mutex::new(ReviewCache {
                rows: vec![],
                fetched_at: None,
                fetched_instant: None,
                in_flight: false,
                last_error: None,
            })),
        }
    }

    pub fn review_adapter(&self, source: &str) -> Option<&dyn ReviewAdapter> {
        self.review_adapters
            .iter()
            .find(|a| a.source() == source)
            .map(|b| b.as_ref())
    }

    pub fn agent_adapter(&self, name: &str) -> Option<&dyn AgentAdapter> {
        self.agent_adapters
            .iter()
            .find(|a| a.name() == name)
            .map(|b| b.as_ref())
    }

    /// Registry-known `pid -> port` for fleet-started agents (matched by port being alive).
    async fn known_ports(&self, procs: &ProcTable) -> HashMap<u32, u16> {
        let mut map = HashMap::new();
        let Ok(reg) = Registry::open() else {
            return map;
        };
        let Ok(tasks) = reg.open_tasks() else {
            return map;
        };
        // Map port -> pid via the processes' listening ports.
        let pids: Vec<u32> = self
            .agent_adapters
            .iter()
            .flat_map(|a| {
                procs
                    .named(if a.name() == "opencode" {
                        "opencode"
                    } else {
                        a.name()
                    })
                    .into_iter()
                    .map(|p| p.pid)
                    .collect::<Vec<_>>()
            })
            .collect();
        let ports = agents::opencode::plannotator::listening_ports(&pids).await;
        for t in tasks {
            if let Some(port) = t.port {
                if let Some((pid, _)) = ports.iter().find(|(_, ps)| ps.contains(&port)) {
                    map.insert(*pid, port);
                }
            }
        }
        map
    }

    /// One full snapshot. Never fails on a collector problem: it reports it in `warnings`.
    pub async fn snapshot(&self, wait_for_reviews: bool) -> Fleet {
        let mut warnings = vec![];
        let procs = tokio::task::spawn_blocking(ProcTable::snapshot).await.ok();

        // Panes are needed both by the join and (as title hints) by the agent adapters.
        let panes_for_hints = tmux::list_panes().await.unwrap_or_default();
        let panes_f = async { Ok::<_, anyhow::Error>(panes_for_hints.clone()) };
        let worktrees_f = async {
            let mut all: Vec<WorktreeRow> = vec![];
            let mut errs = vec![];
            for repo in &self.config.repos {
                match git::worktrees(repo).await {
                    Ok(v) => all.extend(v),
                    Err(e) => errs.push(format!("{}: {e}", repo.name)),
                }
            }
            (all, errs)
        };
        let agents_f = async {
            let mut all = vec![];
            let mut errs = vec![];
            if let Some(p) = procs.as_ref() {
                let known_ports = self.known_ports(p).await;
                let title_hints = title_hints(p, &panes_for_hints).await;
                let ctx = DiscoverCtx {
                    known_ports,
                    title_hints,
                };
                for a in &self.agent_adapters {
                    match a.discover(p, &ctx).await {
                        Ok(v) => all.extend(v),
                        Err(e) => errs.push(format!("{}: {e}", a.name())),
                    }
                }
            }
            (all, errs)
        };
        let reviews_f = self.reviews_cached(wait_for_reviews);

        let (panes, (worktrees, wt_errs), (agents, ag_errs), (reviews, fetched_at, rv_err)) =
            tokio::join!(panes_f, worktrees_f, agents_f, reviews_f);

        let panes = match panes {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("tmux: {e}"));
                vec![]
            }
        };
        warnings.extend(wt_errs);
        warnings.extend(ag_errs);
        if let Some(e) = rv_err {
            warnings.push(format!("reviews: {e}"));
        }

        let mut fleet = join::join(Inputs {
            panes,
            agents,
            worktrees,
            reviews,
            procs: procs.as_ref(),
        });
        fleet.reviews_fetched_at = fetched_at;
        fleet.warnings = warnings;
        fleet
    }

    /// Returns the cached reviews, kicking off a background refresh when stale. With
    /// `wait` the first fetch is awaited (one-shot CLI commands).
    async fn reviews_cached(
        &self,
        wait: bool,
    ) -> (Vec<ReviewRow>, Option<DateTime<Utc>>, Option<String>) {
        let ttl = Duration::from_secs(self.config.review_ttl_secs);
        let stale = {
            let c = self.reviews.lock().await;
            c.fetched_instant.map(|t| t.elapsed() > ttl).unwrap_or(true) && !c.in_flight
        };
        if stale {
            if wait || self.reviews.lock().await.fetched_instant.is_none() && wait {
                self.fetch_reviews().await;
            } else {
                self.spawn_fetch();
            }
        }
        let c = self.reviews.lock().await;
        (c.rows.clone(), c.fetched_at, c.last_error.clone())
    }

    pub fn spawn_fetch(&self) {
        // Adapters are not `'static`; rebuild them for the background task from config.
        let sources: Vec<String> = self
            .review_adapters
            .iter()
            .map(|a| a.source().to_string())
            .collect();
        let cache = self.reviews.clone();
        tokio::spawn(async move {
            {
                let mut c = cache.lock().await;
                if c.in_flight {
                    return;
                }
                c.in_flight = true;
            }
            let adapters: Vec<Box<dyn ReviewAdapter>> = sources
                .iter()
                .filter_map(|s| reviews::adapter_for(s).ok())
                .collect();
            let (rows, err) = fetch_all(&adapters).await;
            let mut c = cache.lock().await;
            if let Some(r) = rows {
                c.rows = r;
                c.fetched_at = Some(Utc::now());
                c.fetched_instant = Some(Instant::now());
            }
            c.last_error = err;
            c.in_flight = false;
        });
    }

    async fn fetch_reviews(&self) {
        {
            let mut c = self.reviews.lock().await;
            if c.in_flight {
                return;
            }
            c.in_flight = true;
        }
        let (rows, err) = fetch_all(&self.review_adapters).await;
        let mut c = self.reviews.lock().await;
        if let Some(r) = rows {
            c.rows = r;
            c.fetched_at = Some(Utc::now());
            c.fetched_instant = Some(Instant::now());
        }
        c.last_error = err;
        c.in_flight = false;
    }

    /// Force a refresh now (key `R`).
    pub async fn refresh_reviews(&self) {
        self.reviews.lock().await.fetched_instant = None;
        self.fetch_reviews().await;
    }
}

/// `pid -> pane title` for every agent process whose ancestor is a tmux pane shell.
async fn title_hints(procs: &ProcTable, panes: &[crate::model::PaneRow]) -> HashMap<u32, String> {
    let by_pid: HashMap<u32, usize> = panes.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    let mut out = HashMap::new();
    for p in procs.named("opencode") {
        if let Some(i) = procs.ancestor_in(p.pid, &by_pid) {
            out.insert(p.pid, panes[i].title.clone());
        }
    }
    out
}

async fn fetch_all(
    adapters: &[Box<dyn ReviewAdapter>],
) -> (Option<Vec<ReviewRow>>, Option<String>) {
    if adapters.is_empty() {
        return (Some(vec![]), None);
    }
    let results = futures::future::join_all(adapters.iter().map(|a| a.open_reviews())).await;
    let mut rows = vec![];
    let mut errs = vec![];
    let mut any_ok = false;
    for (a, r) in adapters.iter().zip(results) {
        match r {
            Ok(v) => {
                any_ok = true;
                rows.extend(v)
            }
            Err(e) => errs.push(format!("{}: {e}", a.source())),
        }
    }
    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let err = if errs.is_empty() {
        None
    } else {
        Some(errs.join("; "))
    };
    (if any_ok { Some(rows) } else { None }, err)
}

/// Convenience for one-shot commands.
pub async fn once(config: Config) -> Result<Fleet> {
    Ok(Engine::new(config).snapshot(true).await)
}
