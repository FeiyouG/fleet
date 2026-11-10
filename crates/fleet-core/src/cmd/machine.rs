//! `fleet machine …`: list and control the machines of every configured profile.
//!
//! Fleet never pauses or stops a machine on its own (spec: machines); these are the verbs you
//! (or the TUI) use. A verb the provider can't do reports "not supported".

use crate::config::{Config, MachineConfig};
use crate::machines::{adapter_for, MachineAdapter, MachineRow};
use crate::registry::Registry;
use anyhow::{bail, Context, Result};
use std::time::Duration;

pub use crate::model::MachineView as Listed;

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

/// The in-machine publisher (`fleet machine publish`, started by `configure_fleet`): local
/// snapshots, and a metadata patch only when a task's state changed. Checks every
/// `publish_secs` while an agent works, six times less often while all of them wait.
pub async fn publish_loop(once: bool) -> Result<()> {
    use crate::machines::remote::MachineFile;
    use crate::machines::state::{diff, published_for, slug_of, wt_dir, Published, KEY_PREFIX};
    let mf = MachineFile::load()?;
    let cfg = Config::load()?;
    let profile = MachineConfig {
        name: "self".into(),
        provider: mf.provider.clone(),
        api_key_env: Some(mf.api_key_env.clone()),
        api_url: mf.api_url.clone(),
        template: None,
        snapshot: None,
        size: None,
        per_task: true,
        idle_min: None,
        max_run_min: None,
        secrets: vec![],
        files: Default::default(),
        env: Default::default(),
        workdir: Some(mf.workdir.clone()),
        mosh: false,
        publish_key: None,
        publish_secs: mf.publish_secs,
        fleet_source: String::new(),
        fleet_ref: String::new(),
        setup: vec![],
    };
    let adapter = adapter_for(&profile)?;
    let engine = crate::snapshot::Engine::new(cfg).merged_lookups(false);
    let wt = wt_dir(&mf.workdir);
    let mut prev: std::collections::BTreeMap<String, String> = adapter
        .get(&mf.id)
        .await?
        .metadata
        .into_iter()
        .filter(|(k, _)| k.starts_with(KEY_PREFIX))
        .collect();
    loop {
        let f = engine.snapshot(false).await;
        let mut next = std::collections::BTreeMap::new();
        let mut busy = false;
        for v in &f.worktrees {
            let Some(slug) = slug_of(&v.worktree.path, &wt) else {
                continue;
            };
            let agent = v.agents.first();
            busy |= agent
                .map(|a| a.activity == crate::model::Activity::Working)
                .unwrap_or(false);
            next.insert(
                Published::key(&slug),
                published_for(agent, Some(&v.worktree)).encode(),
            );
        }
        // Only state worth reporting changes the key; a fresh timestamp alone does not.
        let strip = |m: &std::collections::BTreeMap<String, String>| {
            m.iter()
                .map(|(k, v)| {
                    let mut p = Published::decode(v).unwrap_or_default();
                    p.u = None;
                    (k.clone(), p.encode())
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let d = diff(&strip(&prev), &strip(&next));
        if !d.is_empty() {
            let patch: std::collections::BTreeMap<String, String> = d
                .keys()
                .map(|k| (k.clone(), next.get(k).cloned().unwrap_or_default()))
                .collect();
            match adapter.publish(&mf.id, &patch).await {
                Ok(()) => prev = next,
                Err(e) => eprintln!("publish: {e:#}"),
            }
        }
        if once {
            return Ok(());
        }
        let secs = if busy {
            mf.publish_secs
        } else {
            mf.publish_secs * 6
        };
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
}

/// `fleet machine snapshot PROFILE --project P`: a fresh machine from the profile's
/// template, prepared for the project, with fleet built from `fleet_source`/`fleet_ref`, your
/// opencode config (no logins), `fleet setup` and the profile's `setup` commands; saved as
/// snapshot `name`; the build machine is then deleted. Returns the snapshot id.
pub async fn build_snapshot(
    cfg: &Config,
    profile: &str,
    project: &str,
    name: &str,
    log: &super::Progress,
) -> Result<String> {
    use crate::machines::remote;
    let mut p = cfg
        .machine(profile)
        .with_context(|| format!("no machine profile `{profile}`"))?;
    if p.provider == "local" {
        bail!("`{profile}` is this computer; snapshots are for cloud profiles");
    }
    let proj = cfg
        .project(project)
        .with_context(|| format!("no project `{project}`"))?;
    let agent = cfg
        .agent(&cfg.defaults_for(Some(proj)).agent, Some(proj))
        .context("defaults.agent is not defined")?;
    let origin = crate::worktrees::git::remote_url(&proj.repo, "origin").await?;
    p.snapshot = None; // always from the template
    let a = adapter_for(&p)?;
    let build_name = remote::machine_name(profile, "snapshot-build");
    log(&format!("create build machine {build_name}"));
    let m = a.create(&remote::spec_for(&p, &build_name)).await?;
    let result = async {
        let on = remote::On {
            adapter: a.as_ref(),
            id: m.id.clone(),
        };
        let layout = remote::Layout::new(&p, project);
        log("prepare (tmux, git, clone)");
        remote::prepare(&on, &layout, &origin).await?;
        remote::configure_fleet(&on, &p, &layout, project, &agent).await?;

        log(&format!(
            "install fleet from {} ({}) — a few minutes",
            p.fleet_source, p.fleet_ref
        ));
        let install = format!(
            "command -v fleet >/dev/null 2>&1 || cargo install --locked --git {} --branch {} fleet",
            shell_words::quote(&p.fleet_source),
            shell_words::quote(&p.fleet_ref)
        );
        background(&on, "install-fleet", &install, Duration::from_secs(40 * 60)).await?;

        let oc = crate::config::xdg_config_home().join("opencode");
        if oc.is_dir() {
            log(&format!("copy {} (no auth files)", oc.display()));
            let tgz = std::env::temp_dir().join(format!("fleet-opencode-{}.tgz", std::process::id()));
            let st = tokio::process::Command::new("tar")
                .args([
                    "czf",
                    &tgz.to_string_lossy(),
                    "--exclude",
                    "node_modules",
                    "--exclude",
                    "*auth*.json",
                    "--exclude",
                    ".git",
                    "-C",
                    &oc.parent().unwrap().to_string_lossy(),
                    "opencode",
                ])
                .status()
                .await?;
            if !st.success() {
                bail!("tar of {} failed", oc.display());
            }
            let bytes = std::fs::read(&tgz)?;
            std::fs::remove_file(&tgz).ok();
            a.write_file(&m.id, "/data/home/.fleet-opencode.tgz", bytes).await?;
            on.bash("mkdir -p ~/.config && tar xzf ~/.fleet-opencode.tgz -C ~/.config && rm ~/.fleet-opencode.tgz")
                .await?;
        }
        log("fleet setup");
        on.bash("fleet setup >/dev/null 2>&1 || true").await?;
        for (i, c) in p.setup.iter().enumerate() {
            log(&format!("setup {}: {c}", i + 1));
            background(&on, &format!("setup-{i}"), &format!("cd {} && {c}", shell_words::quote(&layout.repo.to_string_lossy())), Duration::from_secs(60 * 60)).await?;
        }
        // The build task's own state must not ride along into every machine.
        on.bash("rm -f ~/.config/fleet/machine.json; pkill -f 'fleet machine publish' || true")
            .await?;
        log(&format!("save snapshot {name}"));
        a.snapshot(&m.id, name).await
    }
    .await;
    log(&format!("delete build machine {build_name}"));
    let del = a.destroy(&m.id).await;
    let id = result?;
    del?;
    Ok(id)
}

/// Run a long command detached on the machine (exec is capped at 10 minutes) and wait for it.
async fn background(
    on: &crate::machines::remote::On<'_>,
    tag: &str,
    cmd: &str,
    timeout: Duration,
) -> Result<()> {
    let dir = format!("$HOME/.local/share/fleet/jobs/{tag}");
    on.bash(&format!(
        "mkdir -p {dir} && rm -f {dir}/rc && (nohup bash -lc {} >{dir}/log 2>&1; echo $? >{dir}/rc) </dev/null >/dev/null 2>&1 &",
        shell_words::quote(cmd)
    ))
    .await?;
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let rc = on
            .bash(&format!("cat {dir}/rc 2>/dev/null || true"))
            .await?;
        let rc = rc.trim();
        if !rc.is_empty() {
            if rc != "0" {
                let tail = on
                    .bash(&format!("tail -20 {dir}/log"))
                    .await
                    .unwrap_or_default();
                bail!("`{cmd}` failed ({rc}):\n{tail}");
            }
            return Ok(());
        }
        if started.elapsed() > timeout {
            bail!("`{cmd}` still running after {timeout:?}");
        }
    }
}

/// Logins with every refresh token removed, so only the laptop ever refreshes them.
pub fn strip_refresh(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            m.retain(|k, _| {
                let k = k.to_ascii_lowercase().replace(['_', '-'], "");
                k != "refresh" && k != "refreshtoken"
            });
            for x in m.values_mut() {
                strip_refresh(x);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(strip_refresh),
        _ => {}
    }
}

/// `fleet creds sync`: push each profile's `files` (laptop login files) to the provider as
/// secrets, refresh tokens removed. Returns `(secret, pushed)` per file; unchanged files
/// (same content as the last push) are skipped.
pub async fn creds_sync(cfg: &Config, only: Option<&str>) -> Result<Vec<(String, bool)>> {
    let mut out = vec![];
    let state_dir = crate::config::data_dir().join("creds");
    std::fs::create_dir_all(&state_dir)?;
    for p in cfg.machine_profiles() {
        if p.provider == "local" || only.map(|o| o != p.name).unwrap_or(false) {
            continue;
        }
        let a = adapter_for(&p)?;
        for (rel, secret) in &p.files {
            let local = crate::config::home().join(rel);
            let Ok(text) = std::fs::read_to_string(&local) else {
                continue;
            };
            let value = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(mut v) => {
                    strip_refresh(&mut v);
                    serde_json::to_string(&v)?
                }
                Err(_) => text,
            };
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            value.hash(&mut h);
            let digest = format!("{:x}", h.finish());
            let mark = state_dir.join(format!("{}-{secret}", p.name));
            if std::fs::read_to_string(&mark).ok().as_deref() == Some(digest.as_str()) {
                out.push((secret.clone(), false));
                continue;
            }
            a.put_secret(secret, &value)
                .await
                .with_context(|| format!("secret {secret} ({})", local.display()))?;
            std::fs::write(&mark, digest)?;
            out.push((secret.clone(), true));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn refresh_tokens_are_removed() {
        let mut v = serde_json::json!({
            "anthropic": {"type": "oauth", "access": "a", "refresh": "r", "expires": 1},
            "mcp": {"grafana": {"tokens": {"accessToken": "x", "refreshToken": "y", "refresh_token": "z"}}}
        });
        super::strip_refresh(&mut v);
        let s = v.to_string();
        assert!(
            !s.contains("\"r\"") && !s.contains("\"y\"") && !s.contains("\"z\""),
            "{s}"
        );
        assert!(s.contains("\"access\":\"a\"") && s.contains("\"accessToken\":\"x\""));
    }
}

#[cfg(test)]
mod tests_durations {
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
