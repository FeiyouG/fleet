//! `fleet machine …`: list and control the machines of every configured profile.
//!
//! Fleet never pauses or stops a machine on its own (spec: machines); these are the verbs you
//! (or the TUI) use. A verb the provider can't do reports "not supported".

use crate::config::{Config, MachineConfig};
use crate::machines::{adapter_for, MachineAdapter, MachineRow};
use crate::registry::Registry;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::time::Duration;

/// A machine with the profile that reaches it and the open tasks on it.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub profile: String,
    #[serde(flatten)]
    pub row: MachineRow,
    pub tasks: Vec<String>,
}

/// One adapter per distinct provider endpoint (several profiles can share an InstaBox
/// workspace: list it once).
fn endpoints(cfg: &Config) -> Vec<MachineConfig> {
    let mut seen = vec![];
    let mut out = vec![];
    for p in cfg.machine_profiles() {
        let key = (
            p.provider.clone(),
            p.api_url.clone(),
            p.api_key_env.clone(),
            if p.provider == "local" {
                Some(p.name.clone())
            } else {
                None
            },
        );
        if !seen.contains(&key) {
            seen.push(key);
            out.push(p);
        }
    }
    out
}

/// Every machine of every profile. A provider that fails (no key, offline) is reported in
/// the returned warnings, not as an error.
pub async fn list(cfg: &Config) -> (Vec<Listed>, Vec<String>) {
    let tasks = Registry::open()
        .and_then(|r| r.open_tasks())
        .unwrap_or_default();
    let mut out = vec![];
    let mut warnings = vec![];
    for p in endpoints(cfg) {
        let rows = match adapter_for(&p) {
            Ok(a) => a.list().await,
            Err(e) => Err(e),
        };
        match rows {
            Ok(rows) => {
                for row in rows {
                    // Cloud machines fleet didn't make (no profile label) are still shown.
                    let profile = row.profile().unwrap_or(&p.name).to_string();
                    let on: Vec<String> = tasks
                        .iter()
                        .filter(|t| t.machine_id == row.id)
                        .map(|t| t.slug.clone())
                        .collect();
                    out.push(Listed {
                        profile,
                        row,
                        tasks: on,
                    });
                }
            }
            Err(e) => warnings.push(format!("machines.{} ({}): {e:#}", p.name, p.provider)),
        }
    }
    (out, warnings)
}

/// The machine `target` (id or name), with an adapter that reaches it.
pub async fn find(
    cfg: &Config,
    target: &str,
) -> Result<(MachineConfig, Box<dyn MachineAdapter>, MachineRow)> {
    let mut errors = vec![];
    for p in endpoints(cfg) {
        let a = adapter_for(&p)?;
        match a.list().await {
            Ok(rows) => {
                if let Some(r) = rows
                    .into_iter()
                    .find(|r| r.id == target || r.name == target)
                {
                    return Ok((p, a, r));
                }
            }
            Err(e) => errors.push(format!("{}: {e:#}", p.name)),
        }
    }
    if errors.is_empty() {
        bail!("no machine `{target}` (see `fleet machine ls`)")
    }
    bail!(
        "no machine `{target}` found; some providers failed: {}",
        errors.join("; ")
    )
}

/// `30s` `10m` `2h` `1d`; `off` / `0` = none.
pub fn parse_duration(s: &str) -> Result<Option<Duration>> {
    let s = s.trim();
    if s == "off" || s == "0" || s == "never" {
        return Ok(None);
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .with_context(|| format!("`{s}`: expected a duration like 30m, 2h or off"))?;
    let secs = match unit {
        "" | "m" | "min" => n * 60,
        "s" => n,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("`{s}`: unknown unit `{unit}` (use s, m, h or d)"),
    };
    Ok(Some(Duration::from_secs(secs)))
}

pub fn fmt_duration(d: Option<Duration>) -> String {
    match d {
        None => "—".into(),
        Some(d) => {
            let s = d.as_secs();
            if s % 3600 == 0 && s >= 3600 {
                format!("{}h", s / 3600)
            } else if s % 60 == 0 {
                format!("{}m", s / 60)
            } else {
                format!("{s}s")
            }
        }
    }
}

/// Create a machine from a profile (what a task start does, without the task).
pub async fn create(cfg: &Config, profile: &str, name: Option<&str>) -> Result<MachineRow> {
    let p = cfg
        .machine(profile)
        .with_context(|| format!("no machine profile `{profile}`"))?;
    if p.provider == "local" {
        bail!("`{profile}` is this computer; nothing to create");
    }
    let name = name
        .map(String::from)
        .unwrap_or_else(|| crate::machines::remote::machine_name(profile, &crate::slug::temp()));
    let a = adapter_for(&p)?;
    a.create(&crate::machines::remote::spec_for(&p, &name))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("off").unwrap(), None);
        assert_eq!(
            parse_duration("30").unwrap(),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(
            parse_duration("2h").unwrap(),
            Some(Duration::from_secs(7200))
        );
        assert_eq!(
            parse_duration("90s").unwrap(),
            Some(Duration::from_secs(90))
        );
        assert!(parse_duration("2w").is_err());
        assert!(parse_duration("abc").is_err());
        assert_eq!(fmt_duration(Some(Duration::from_secs(7200))), "2h");
        assert_eq!(fmt_duration(Some(Duration::from_secs(600))), "10m");
        assert_eq!(fmt_duration(None), "—");
    }
}
