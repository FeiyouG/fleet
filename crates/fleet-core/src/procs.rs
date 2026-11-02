//! Process tree: find agent processes, their cwd, and which tmux pane hosts them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub cmdline: Vec<String>,
}

pub struct ProcTable {
    by_pid: HashMap<u32, Proc>,
}

impl ProcTable {
    pub fn snapshot() -> Self {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cwd(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        );
        let by_pid = sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let pid = pid.as_u32();
                (
                    pid,
                    Proc {
                        pid,
                        ppid: p.parent().map(Pid::as_u32),
                        name: p.name().to_string_lossy().into_owned(),
                        cwd: p.cwd().map(Path::to_path_buf),
                        cmdline: p
                            .cmd()
                            .iter()
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect(),
                    },
                )
            })
            .collect();
        Self { by_pid }
    }

    pub fn from_procs(procs: Vec<Proc>) -> Self {
        Self {
            by_pid: procs.into_iter().map(|p| (p.pid, p)).collect(),
        }
    }

    pub fn get(&self, pid: u32) -> Option<&Proc> {
        self.by_pid.get(&pid)
    }

    /// Processes whose name equals `name` (exact, e.g. `opencode`).
    pub fn named(&self, name: &str) -> Vec<&Proc> {
        let mut v: Vec<_> = self.by_pid.values().filter(|p| p.name == name).collect();
        v.sort_by_key(|p| p.pid);
        v
    }

    /// Walk up the parent chain from `pid`; return the first ancestor (or self) whose pid
    /// is in `candidates`. Used to map an agent process to the tmux pane shell that spawned it.
    pub fn ancestor_in(&self, pid: u32, candidates: &HashMap<u32, usize>) -> Option<usize> {
        let mut cur = Some(pid);
        let mut hops = 0;
        while let Some(p) = cur {
            if let Some(&i) = candidates.get(&p) {
                return Some(i);
            }
            cur = self
                .by_pid
                .get(&p)
                .and_then(|x| x.ppid)
                .filter(|&pp| pp != p && pp != 0);
            hops += 1;
            if hops > 64 {
                break;
            }
        }
        None
    }

    /// `pid` and every ancestor of it, nearest first.
    pub fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut v = vec![];
        let mut cur = Some(pid);
        while let Some(p) = cur {
            if v.contains(&p) || v.len() > 64 {
                break;
            }
            v.push(p);
            cur = self
                .by_pid
                .get(&p)
                .and_then(|x| x.ppid)
                .filter(|&pp| pp != 0);
        }
        v
    }

    pub fn is_alive(&self, pid: u32) -> bool {
        self.by_pid.contains_key(&pid)
    }

    /// `pid` plus every process below it in the tree.
    pub fn descendants(&self, pid: u32) -> Vec<u32> {
        let mut out = vec![pid];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i];
            for p in self.by_pid.values() {
                if p.ppid == Some(cur) && !out.contains(&p.pid) {
                    out.push(p.pid);
                }
            }
            i += 1;
        }
        out
    }
}

/// Canonicalise, falling back to the input when the path no longer exists.
pub fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Canonicalise the parent only, keeping the last component as written. Needed for a path
/// that is itself a symlink (a renamed worktree's old path): `canon` would follow it.
pub fn canon_keep_leaf(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(leaf)) => canon(parent).join(leaf),
        _ => canon(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_walk() {
        let t = ProcTable::from_procs(vec![
            Proc {
                pid: 1,
                ppid: None,
                name: "launchd".into(),
                cwd: None,
                cmdline: vec![],
            },
            Proc {
                pid: 10,
                ppid: Some(1),
                name: "zsh".into(),
                cwd: None,
                cmdline: vec![],
            },
            Proc {
                pid: 20,
                ppid: Some(10),
                name: "opencode".into(),
                cwd: None,
                cmdline: vec![],
            },
            Proc {
                pid: 30,
                ppid: Some(20),
                name: "bun".into(),
                cwd: None,
                cmdline: vec![],
            },
        ]);
        let mut c = HashMap::new();
        c.insert(10u32, 7usize);
        assert_eq!(t.ancestor_in(30, &c), Some(7));
        assert_eq!(t.ancestor_in(20, &c), Some(7));
        assert_eq!(t.ancestor_in(1, &c), None);
    }
}
