//! Run every collector, join, return a `Fleet`. Reviews are cached for `review_ttl_secs`
//! and refreshed in the background so the UI never waits on the forge.

use crate::agents::{self, AgentAdapter, DiscoverCtx};
use crate::config::Config;
use crate::join::{self, Inputs};
use crate::model::{Fleet, ReviewRow, WorktreeRow};
use crate::procs::{canon, canon_keep_leaf, ProcTable};
use crate::registry::Registry;
use crate::reviews::{self, ReviewAdapter};
use crate::tmux;
use crate::worktrees::git;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
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

/// `(source, branch)` -> when looked up, and the merged review found (if any).
type MergedCache = HashMap<(String, String), (Instant, Option<ReviewRow>)>;

pub struct Engine {
    pub config: Config,
    agent_adapters: Vec<Box<dyn AgentAdapter>>,
    review_adapters: Vec<Box<dyn ReviewAdapter>>,
    reviews: Arc<Mutex<ReviewCache>>,
    merged: Arc<Mutex<MergedCache>>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        let agent_adapters = agents::adapters(&config.agents, &config.agent_args);
        let mut sources: Vec<String> = config
            .projects
            .iter()
            .filter_map(|r| r.reviews.clone())
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
            merged: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn agent_adapters(&self) -> &[Box<dyn AgentAdapter>] {
        &self.agent_adapters
    }

    /// Every pid that belongs to an enabled adapter.
    fn agent_pids(&self, procs: &ProcTable) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .agent_adapters
            .iter()
            .flat_map(|a| {
                a.process_names()
                    .iter()
                    .flat_map(|n| procs.named(n).into_iter().map(|p| p.pid))
                    .collect::<Vec<_>>()
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Registry facts: `pid -> port` for fleet-started agents (matched by the port being
    /// alive in that process) and `old path -> current path` for renamed tasks.
    fn registry_ctx(
        &self,
        ports: &HashMap<u32, Vec<u16>>,
    ) -> (HashMap<u32, u16>, HashMap<PathBuf, PathBuf>) {
        let mut map = HashMap::new();
        let mut aliases = HashMap::new();
        let Ok(reg) = Registry::open() else {
            return (map, aliases);
        };
        let Ok(tasks) = reg.open_tasks() else {
            return (map, aliases);
        };
        for t in tasks {
            if let Some(port) = t.port {
                if let Some((pid, _)) = ports.iter().find(|(_, ps)| ps.contains(&port)) {
                    map.insert(*pid, port);
                }
            }
            let cur = canon(&t.path);
            for old in &t.old_paths {
                aliases.insert(canon_keep_leaf(old), cur.clone());
            }
        }
        (map, aliases)
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
            for repo in &self.config.projects {
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
                let pids = self.agent_pids(p);
                // Listening ports of each agent pid and its descendants (Claude/Codex run
                // Plannotator as a hook child), keyed by the agent pid.
                let mut fam: Vec<(u32, Vec<u32>)> =
                    pids.iter().map(|&pid| (pid, p.descendants(pid))).collect();
                let all_pids: Vec<u32> = fam.iter().flat_map(|(_, d)| d.iter().copied()).collect();
                let raw = agents::opencode::plannotator::listening_ports(&all_pids).await;
                let mut ports: HashMap<u32, Vec<u16>> = HashMap::new();
                for (pid, d) in fam.drain(..) {
                    let mut v: Vec<u16> = d
                        .iter()
                        .flat_map(|x| raw.get(x).cloned().unwrap_or_default())
                        .collect();
                    v.sort();
                    v.dedup();
                    ports.insert(pid, v);
                }
                let (known_ports, path_aliases) = self.registry_ctx(&ports);
                let title_hints = title_hints(p, &pids, &panes_for_hints);
                let states = agents::state_file::read_all(p);
                let ctx = DiscoverCtx {
                    known_ports,
                    title_hints,
                    path_aliases,
                    states,
                    ports,
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

        let repo_sources: HashMap<String, String> = self
            .config
            .projects
            .iter()
            .filter_map(|r| r.reviews.clone().map(|s| (r.name.clone(), s)))
            .collect();
        // Local branches without an open review: was it merged? (gc and close need to know.)
        let wanted: Vec<(String, String)> = worktrees
            .iter()
            .filter(|w| !w.is_main)
            .filter_map(|w| Some((repo_sources.get(&w.repo)?.clone(), w.branch.clone()?)))
            .filter(|(src, b)| {
                !reviews
                    .iter()
                    .any(|r| r.source == *src && r.source_branch == *b)
            })
            .collect();
        let merged = self.merged_cached(wanted, wait_for_reviews).await;
        let mut fleet = join::join(Inputs {
            panes,
            agents,
            worktrees,
            reviews,
            merged,
            procs: procs.as_ref(),
            repo_sources,
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
        let ttl = Duration::from_secs(self.config.defaults.review_ttl_secs);
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

    /// Merged reviews for `(source, branch)` pairs from the cache; stale or missing entries
    /// are looked up in the background (awaited when `wait`, for one-shot CLI commands).
    async fn merged_cached(&self, wanted: Vec<(String, String)>, wait: bool) -> Vec<ReviewRow> {
        let ttl = Duration::from_secs(self.config.defaults.review_ttl_secs);
        let stale: Vec<(String, String)> = {
            let c = self.merged.lock().await;
            wanted
                .iter()
                .filter(|k| c.get(*k).map(|(t, _)| t.elapsed() > ttl).unwrap_or(true))
                .cloned()
                .collect()
        };
        if !stale.is_empty() {
            let cache = self.merged.clone();
            let job = async move {
                {
                    // Claim the keys so concurrent snapshots do not look them up again.
                    let mut c = cache.lock().await;
                    for k in &stale {
                        let prev = c.get(k).and_then(|(_, r)| r.clone());
                        c.insert(k.clone(), (Instant::now(), prev));
                    }
                }
                let lookups = stale.iter().map(|(src, b)| async move {
                    let r = match reviews::adapter_for(src) {
                        Ok(a) => a.merged_for_branch(b).await,
                        Err(e) => Err(e),
                    };
                    ((src.clone(), b.clone()), r)
                });
                for (k, r) in futures::future::join_all(lookups).await {
                    let mut c = cache.lock().await;
                    // On an error keep the old answer; it is retried after the TTL.
                    if let Ok(v) = r {
                        c.insert(k, (Instant::now(), v));
                    }
                }
            };
            if wait {
                job.await;
            } else {
                tokio::spawn(job);
            }
        }
        let c = self.merged.lock().await;
        wanted
            .iter()
            .filter_map(|k| c.get(k).and_then(|(_, r)| r.clone()))
            .collect()
    }

    /// A live lookup (no cache) of the merged review of `branch` in `source`.
    pub async fn merged_review(&self, source: &str, branch: &str) -> Result<Option<ReviewRow>> {
        let r = reviews::adapter_for(source)?
            .merged_for_branch(branch)
            .await?;
        self.merged.lock().await.insert(
            (source.to_string(), branch.to_string()),
            (Instant::now(), r.clone()),
        );
        Ok(r)
    }

    /// Force a refresh now (key `R`).
    pub async fn refresh_reviews(&self) {
        self.reviews.lock().await.fetched_instant = None;
        self.merged.lock().await.clear();
        self.fetch_reviews().await;
    }
}

/// `pid -> pane title` for every agent process whose ancestor is a tmux pane shell.
fn title_hints(
    procs: &ProcTable,
    pids: &[u32],
    panes: &[crate::model::PaneRow],
) -> HashMap<u32, String> {
    let by_pid: HashMap<u32, usize> = panes.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    let mut out = HashMap::new();
    for &pid in pids {
        if let Some(i) = procs.ancestor_in(pid, &by_pid) {
            out.insert(pid, panes[i].title.clone());
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
