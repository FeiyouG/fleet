//! Read-only view of `~/.local/share/opencode/opencode.db`.
//!
//! Only three facts per session are needed:
//! - the session (id, title, directory, mode, last update)
//! - whether the last assistant message is completed (idle) or not (working)
//! - the tool part still `running` on that message, if any (what it is blocked on)

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DbSession {
    pub id: String,
    pub title: String,
    pub directory: PathBuf,
    pub mode: Option<String>,
    pub updated: DateTime<Utc>,
    /// `true` when the last assistant message has `time.completed`.
    pub last_completed: bool,
    pub running_tool: Option<String>,
}

pub fn default_db_path() -> PathBuf {
    crate::config::xdg_data_home()
        .join("opencode")
        .join("opencode.db")
}

/// Sessions updated within `since_secs`, newest first.
pub fn recent_sessions(db: &Path, since_secs: i64) -> Result<Vec<DbSession>> {
    if !db.exists() {
        return Ok(vec![]);
    }
    let uri = format!("file:{}?mode=ro", db.display());
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", db.display()))?;
    conn.busy_timeout(std::time::Duration::from_millis(500))?;

    let cutoff = Utc::now().timestamp_millis() - since_secs * 1000;

    let mut st = conn.prepare(
        "select id, title, directory, agent, time_updated from session \
         where time_updated > ?1 and time_archived is null order by time_updated desc",
    )?;
    let mut sessions: Vec<DbSession> = st
        .query_map([cutoff], |r| {
            Ok(DbSession {
                id: r.get(0)?,
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                directory: PathBuf::from(r.get::<_, String>(2)?),
                mode: r.get(3)?,
                updated: Utc
                    .timestamp_millis_opt(r.get::<_, i64>(4)?)
                    .single()
                    .unwrap_or_else(Utc::now),
                last_completed: true,
                running_tool: None,
            })
        })?
        .collect::<Result<_, _>>()?;
    drop(st);
    if sessions.is_empty() {
        return Ok(sessions);
    }

    // Last message per session: role + completed?
    let mut last_msg: HashMap<String, (String, bool)> = HashMap::new();
    {
        let mut st = conn.prepare(
            "with last as (select session_id, max(time_created) t from message \
                           where time_created > ?1 group by session_id) \
             select m.session_id, m.id, json_extract(m.data,'$.role'), \
                    json_extract(m.data,'$.time.completed') is not null \
             from message m join last l on l.session_id = m.session_id and m.time_created = l.t",
        )?;
        let rows = st.query_map([cutoff - 7 * 86_400_000], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?;
        for row in rows {
            let (sid, mid, role, completed) = row?;
            // A pending user message means the agent is about to work: treat as not completed.
            let completed = match role.as_deref() {
                Some("assistant") => completed,
                _ => false,
            };
            last_msg.insert(sid, (mid, completed));
        }
    }

    // Running tool on that last message.
    let mut running: HashMap<String, String> = HashMap::new();
    {
        let mut st = conn.prepare(
            "select session_id, json_extract(data,'$.tool') from part \
             where message_id = ?1 and json_extract(data,'$.type') = 'tool' \
               and json_extract(data,'$.state.status') in ('running','pending') \
             order by time_created desc limit 1",
        )?;
        for s in &sessions {
            if let Some((mid, false)) = last_msg.get(&s.id) {
                let tool: Option<String> = st.query_row([mid], |r| r.get(1)).ok();
                if let Some(t) = tool {
                    running.insert(s.id.clone(), t);
                }
            }
        }
    }

    for s in &mut sessions {
        if let Some((_, completed)) = last_msg.get(&s.id) {
            s.last_completed = *completed;
        }
        s.running_tool = running.get(&s.id).cloned();
    }
    Ok(sessions)
}
