//! Remote agent state without waking machines (spec: machines, "Remote agent state without
//! waking").
//!
//! In the machine, `fleet machine publish` (started by `prepare` when fleet is installed
//! there and the machine holds a provider key) takes a local snapshot every few seconds and,
//! only when something changed, writes one compact metadata entry per task:
//! `fleet.t.<slug>` = `{"s":"wait_plan","t":"Fix …","p":4123,"u":1762…,"b":"fix/a",…}`.
//! On the laptop, [`remote_rows`] turns the registry's remote tasks plus that metadata into
//! ordinary agent and worktree rows tagged with their machine. Without a publisher the rows
//! still appear, with the agent state unknown (`None`) and the machine's state.

use super::interfaces::{MachineRow, MachineState};
use crate::model::{Activity, AgentRow, WorktreeRow};
use crate::registry::Task;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub const KEY_PREFIX: &str = "fleet.t.";

/// What one task looks like, as published (short keys: InstaBox values are ≤ 512 chars).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    /// Activity (`working`, `wait_plan`, …).
    pub s: Option<Activity>,
    /// Title, cut to 80 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    /// Plannotator port in the machine, when a plan waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<u16>,
    /// Mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<String>,
    /// Branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<String>,
    /// Dirty, ahead, behind.
    #[serde(default)]
    pub d: bool,
    #[serde(default)]
    pub a: u32,
    #[serde(default)]
    pub h: u32,
    /// Last agent activity, unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub u: Option<i64>,
}

impl Published {
    pub fn key(slug: &str) -> String {
        let mut k = format!("{KEY_PREFIX}{slug}");
        k.truncate(64);
        k
    }
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn decode(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// The port in a Plannotator URL (`http://localhost:4123/…`).
pub fn plan_port(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split('/').next()?;
    host.rsplit(':').next()?.parse().ok()
}

/// A stable, fake pid for a remote agent row (top bit set, never a real local pid), so row
/// keys survive refreshes.
pub fn remote_pid(machine: &str, slug: &str) -> u32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (machine, slug).hash(&mut h);
    0x8000_0000 | (h.finish() as u32 & 0x7fff_ffff)
}

pub fn is_remote_pid(pid: u32) -> bool {
    pid & 0x8000_0000 != 0
}

/// Agent and worktree rows for the remote tasks, from their machines' metadata.
pub fn remote_rows(tasks: &[Task], machines: &[MachineRow]) -> (Vec<AgentRow>, Vec<WorktreeRow>) {
    let mut agents = vec![];
    let mut worktrees = vec![];
    for t in tasks.iter().filter(|t| t.is_remote()) {
        let Some(m) = machines.iter().find(|m| m.id == t.machine_id) else {
            continue;
        };
        if m.state == MachineState::Gone {
            continue;
        }
        let p = m
            .metadata
            .get(&Published::key(&t.slug))
            .and_then(|v| Published::decode(v))
            .unwrap_or_default();
        let last = p.u.and_then(|u| Utc.timestamp_opt(u, 0).single());
        let name = t
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| t.slug.clone());
        worktrees.push(WorktreeRow {
            machine: Some(m.name.clone()),
            repo: t.repo.clone(),
            path: t.path.clone(),
            name,
            branch: p.b.clone().or_else(|| t.branch.clone()),
            head: t.head_sha.clone().unwrap_or_default(),
            upstream: p.b.as_ref().map(|b| format!("origin/{b}")),
            ahead: p.a,
            behind: p.h,
            dirty: p.d,
            detached: t.branch.is_none() && p.b.is_none(),
            is_main: false,
            missing: false,
            in_base: false,
        });
        agents.push(AgentRow {
            machine: Some(m.name.clone()),
            adapter: t.agent.clone(),
            pid: remote_pid(&m.name, &t.slug),
            cwd: t.path.clone(),
            session_id: None,
            title: p.t.clone(),
            mode: p.m.clone().or_else(|| t.mode.clone()),
            model: None,
            activity: p.s.unwrap_or(Activity::None),
            running_tool: None,
            plan_url: p.p.map(|port| format!("remote:{port}")),
            plan_title: None,
            port: t.port,
            last_activity: last.or(Some(t.created_at)),
            pane_target: Some(format!("{}:1.1", t.tmux_session)),
            pending: vec![],
            control_url: None,
            control_token: None,
        });
    }
    (agents, worktrees)
}

/// What the in-machine publisher writes for one task, from a local snapshot row.
pub fn published_for(agent: Option<&AgentRow>, wt: Option<&WorktreeRow>) -> Published {
    Published {
        s: agent.map(|a| a.activity),
        t: agent
            .and_then(|a| a.title.clone())
            .map(|t| t.chars().take(80).collect()),
        p: agent
            .filter(|a| a.activity == Activity::WaitPlan)
            .and_then(|a| a.plan_url.as_deref())
            .and_then(plan_port),
        m: agent.and_then(|a| a.mode.clone()),
        b: wt.and_then(|w| w.branch.clone()),
        d: wt.map(|w| w.dirty).unwrap_or(false),
        a: wt.map(|w| w.ahead).unwrap_or(0),
        h: wt.map(|w| w.behind).unwrap_or(0),
        u: agent
            .and_then(|a| a.last_activity)
            .map(|d: DateTime<Utc>| d.timestamp()),
    }
}

/// The metadata patch from `prev` to `next` (an empty value deletes the key).
pub fn diff(
    prev: &BTreeMap<String, String>,
    next: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in next {
        if prev.get(k) != Some(v) {
            out.insert(k.clone(), v.clone());
        }
    }
    for k in prev.keys() {
        if !next.contains_key(k) {
            out.insert(k.clone(), String::new());
        }
    }
    out
}

/// The task slug a path under `<workdir>/wt/<slug>` belongs to.
pub fn slug_of(path: &std::path::Path, wt_dir: &std::path::Path) -> Option<String> {
    let rest = path.strip_prefix(wt_dir).ok()?;
    rest.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
}

pub fn wt_dir(workdir: &str) -> PathBuf {
    PathBuf::from(workdir).join("wt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::interfaces::MachineState;

    fn task(slug: &str, machine_id: &str) -> Task {
        Task {
            slug: slug.into(),
            kind: "start".into(),
            repo: "svc".into(),
            branch: Some(format!("feat/{slug}")),
            path: format!("/data/home/src/wt/{slug}").into(),
            old_paths: vec![],
            tmux_session: slug.into(),
            port: None,
            agent: "opencode".into(),
            mode: Some("plan".into()),
            task: None,
            review_source: None,
            review_id: None,
            head_sha: None,
            first_prompt: None,
            created_at: Utc::now(),
            closed_at: None,
            machine: "ibx".into(),
            machine_id: machine_id.into(),
        }
    }

    fn machine(id: &str, meta: BTreeMap<String, String>) -> MachineRow {
        MachineRow {
            provider: "instabox".into(),
            id: id.into(),
            name: format!("ibx-{id}"),
            state: MachineState::Running,
            size: None,
            idle_after: None,
            max_run: None,
            oom_kills: None,
            cost_usd: None,
            metadata: meta,
            created_at: None,
        }
    }

    #[test]
    fn rows_from_metadata_and_without() {
        let p = Published {
            s: Some(Activity::WaitPlan),
            t: Some("Fix it".into()),
            p: Some(4123),
            b: Some("feat/b".into()),
            a: 2,
            ..Default::default()
        };
        let meta: BTreeMap<_, _> = [(Published::key("b"), p.encode())].into();
        let tasks = vec![task("b", "m1"), task("c", "m2"), task("gone", "m9")];
        let ms = vec![machine("m1", meta), machine("m2", BTreeMap::new())];
        let (agents, wts) = remote_rows(&tasks, &ms);
        assert_eq!(agents.len(), 2, "a task whose machine is gone is dropped");
        let b = &agents[0];
        assert_eq!(b.machine.as_deref(), Some("ibx-m1"));
        assert_eq!(b.activity, Activity::WaitPlan);
        assert_eq!(b.plan_url.as_deref(), Some("remote:4123"));
        assert_eq!(b.pane_target.as_deref(), Some("b:1.1"));
        assert!(is_remote_pid(b.pid));
        assert_eq!(b.pid, remote_pid("ibx-m1", "b"));
        assert_eq!(wts[0].ahead, 2);
        assert_eq!(agents[1].activity, Activity::None, "no publisher: unknown");
    }

    #[test]
    fn encode_diff_ports() {
        assert_eq!(plan_port("http://localhost:4123/plan"), Some(4123));
        assert_eq!(plan_port("http://127.0.0.1:80"), Some(80));
        assert_eq!(plan_port("nope"), None);
        let e = Published {
            s: Some(Activity::Working),
            ..Default::default()
        }
        .encode();
        assert!(e.len() < 120, "{e}");
        assert_eq!(Published::decode(&e).unwrap().s, Some(Activity::Working));
        let a: BTreeMap<String, String> =
            [("x".into(), "1".into()), ("y".into(), "2".into())].into();
        let b: BTreeMap<String, String> =
            [("x".into(), "1".into()), ("z".into(), "3".into())].into();
        let d = diff(&a, &b);
        assert_eq!(d.get("y").map(String::as_str), Some(""));
        assert_eq!(d.get("z").map(String::as_str), Some("3"));
        assert!(!d.contains_key("x"));
        assert_eq!(
            slug_of(
                std::path::Path::new("/data/home/src/wt/fix-a/sub"),
                &wt_dir("/data/home/src")
            )
            .as_deref(),
            Some("fix-a")
        );
    }
}
