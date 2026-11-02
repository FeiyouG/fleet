//! `~/.local/share/fleet/fleet.db`: only the rows fleet itself created (start/review).
//! Discovery never needs it; delete it and the list still works.

use crate::config::data_dir;
use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Task {
    pub slug: String,
    /// `start` | `review`
    pub kind: String,
    pub repo: String,
    pub branch: Option<String>,
    pub path: PathBuf,
    pub old_paths: Vec<PathBuf>,
    pub tmux_session: String,
    pub port: Option<u16>,
    /// Adapter that was launched (`opencode`, `claude`, `codex`).
    pub agent: String,
    pub mode: Option<String>,
    pub review_source: Option<String>,
    pub review_id: Option<u64>,
    pub head_sha: Option<String>,
    pub first_prompt: Option<String>,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

pub struct Registry {
    conn: Connection,
}

impl Registry {
    pub fn open() -> Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        Self::open_at(&dir.join("fleet.db"))
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "create table if not exists task (
                slug text primary key,
                kind text not null,
                repo text not null,
                branch text,
                path text not null,
                old_paths text not null default '',
                tmux_session text not null,
                port integer,
                agent text not null default 'opencode',
                mode text,
                review_source text,
                review_id integer,
                head_sha text,
                first_prompt text,
                created_at integer not null,
                closed_at integer
            );",
        )?;
        // v0.3: `agent` column on older databases.
        let has_agent: bool = conn
            .prepare("select count(*) from pragma_table_info('task') where name = 'agent'")?
            .query_row([], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_agent {
            conn.execute_batch(
                "alter table task add column agent text not null default 'opencode';",
            )?;
        }
        Ok(Self { conn })
    }

    pub fn insert(&self, t: &Task) -> Result<()> {
        self.conn.execute(
            "insert or replace into task (slug, kind, repo, branch, path, old_paths, tmux_session, port, agent, mode, \
             review_source, review_id, head_sha, first_prompt, created_at, closed_at) \
             values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                t.slug,
                t.kind,
                t.repo,
                t.branch,
                t.path.to_string_lossy(),
                t.old_paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n"),
                t.tmux_session,
                t.port,
                t.agent,
                t.mode,
                t.review_source,
                t.review_id,
                t.head_sha,
                t.first_prompt,
                t.created_at.timestamp_millis(),
                t.closed_at.map(|d| d.timestamp_millis()),
            ],
        )?;
        Ok(())
    }

    pub fn open_tasks(&self) -> Result<Vec<Task>> {
        let mut st = self.conn.prepare(
            "select slug, kind, repo, branch, path, old_paths, tmux_session, port, mode, review_source, \
             review_id, head_sha, first_prompt, created_at, closed_at, agent from task where closed_at is null",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Task {
                slug: r.get(0)?,
                kind: r.get(1)?,
                repo: r.get(2)?,
                branch: r.get(3)?,
                path: PathBuf::from(r.get::<_, String>(4)?),
                old_paths: r
                    .get::<_, String>(5)?
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(PathBuf::from)
                    .collect(),
                tmux_session: r.get(6)?,
                port: r.get(7)?,
                mode: r.get(8)?,
                review_source: r.get(9)?,
                review_id: r.get(10)?,
                head_sha: r.get(11)?,
                first_prompt: r.get(12)?,
                created_at: Utc
                    .timestamp_millis_opt(r.get::<_, i64>(13)?)
                    .single()
                    .unwrap_or_else(Utc::now),
                closed_at: r
                    .get::<_, Option<i64>>(14)?
                    .and_then(|m| Utc.timestamp_millis_opt(m).single()),
                agent: r
                    .get::<_, Option<String>>(15)?
                    .unwrap_or_else(|| "opencode".into()),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, slug: &str) -> Result<Option<Task>> {
        Ok(self.open_tasks()?.into_iter().find(|t| t.slug == slug))
    }

    pub fn find_review(&self, source: &str, id: u64) -> Result<Option<Task>> {
        Ok(self.open_tasks()?.into_iter().find(|t| {
            t.kind == "review"
                && t.review_source.as_deref() == Some(source)
                && t.review_id == Some(id)
        }))
    }

    pub fn close(&self, slug: &str) -> Result<()> {
        self.conn.execute(
            "update task set closed_at = ?1 where slug = ?2",
            params![Utc::now().timestamp_millis(), slug],
        )?;
        Ok(())
    }

    /// Undo of [`close`](Self::close): the task is open again.
    pub fn reopen(&self, slug: &str) -> Result<()> {
        self.conn.execute(
            "update task set closed_at = null where slug = ?1",
            params![slug],
        )?;
        Ok(())
    }

    /// Open tasks whose current path, or any old (pre-rename) path, is `path`. Paths are
    /// compared canonicalised (registry rows keep `/tmp/...` as written).
    pub fn tasks_for_path(&self, path: &Path) -> Result<Vec<Task>> {
        let want = crate::procs::canon(path);
        Ok(self
            .open_tasks()?
            .into_iter()
            .filter(|t| {
                crate::procs::canon(&t.path) == want
                    || t.path == path
                    || t.old_paths
                        .iter()
                        .any(|o| o == path || crate::procs::canon(o) == want)
            })
            .collect())
    }

    pub fn rename(
        &self,
        old: &str,
        new: &str,
        new_branch: Option<&str>,
        new_path: &Path,
    ) -> Result<()> {
        let Some(t) = self.get(old)? else {
            anyhow::bail!("no registry row for `{old}`")
        };
        let mut old_paths = t.old_paths.clone();
        old_paths.push(t.path.clone());
        self.conn.execute(
            "update task set slug = ?1, branch = ?2, path = ?3, old_paths = ?4, tmux_session = ?1 where slug = ?5",
            params![
                new,
                new_branch.or(t.branch.as_deref()),
                new_path.to_string_lossy(),
                old_paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n"),
                old,
            ],
        )?;
        Ok(())
    }

    pub fn set_head(&self, slug: &str, sha: &str) -> Result<()> {
        self.conn.execute(
            "update task set head_sha = ?1 where slug = ?2",
            params![sha, slug],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_rename() {
        let dir = std::env::temp_dir().join(format!("fleet-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = Registry::open_at(&dir.join("t.db")).unwrap();
        r.insert(&Task {
            slug: "wip-abcd".into(),
            kind: "start".into(),
            repo: "svc".into(),
            branch: Some("wip/wip-abcd".into()),
            path: "/w/wip-abcd".into(),
            old_paths: vec![],
            tmux_session: "wip-abcd".into(),
            port: Some(4096),
            agent: "opencode".into(),
            mode: Some("plan".into()),
            review_source: None,
            review_id: None,
            head_sha: None,
            first_prompt: Some("do it".into()),
            created_at: Utc::now(),
            closed_at: None,
        })
        .unwrap();
        r.rename("wip-abcd", "fix-x", Some("fix/x"), Path::new("/w/fix-x"))
            .unwrap();
        let t = r.get("fix-x").unwrap().unwrap();
        assert_eq!(t.tmux_session, "fix-x");
        assert_eq!(t.old_paths, vec![PathBuf::from("/w/wip-abcd")]);
        assert_eq!(t.port, Some(4096));
        assert_eq!(
            r.tasks_for_path(Path::new("/w/wip-abcd")).unwrap()[0].slug,
            "fix-x"
        );
        assert_eq!(r.tasks_for_path(Path::new("/w/fix-x")).unwrap().len(), 1);
        assert!(r.tasks_for_path(Path::new("/w/other")).unwrap().is_empty());
        r.close("fix-x").unwrap();
        assert!(r.get("fix-x").unwrap().is_none());
        assert!(r.tasks_for_path(Path::new("/w/fix-x")).unwrap().is_empty());
        r.reopen("fix-x").unwrap();
        assert_eq!(r.get("fix-x").unwrap().unwrap().port, Some(4096));
        r.close("fix-x").unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
