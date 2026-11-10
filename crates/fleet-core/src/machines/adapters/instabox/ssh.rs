//! Interactive access with the system `ssh`/`mosh`, no instabox CLI.
//!
//! fleet keeps one ed25519 key pair in `<data>/instabox/`. Per machine it asks
//! `POST /v1/machines/{id}/connections` for a short-lived user certificate for that key and
//! writes the gateway's certificate authority into a per-machine `known_hosts`, so host keys
//! are checked strictly. Certificates are cached until 5 minutes before they expire.

use super::api::Opts;
use super::InstaBox;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

const CERT_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    host: String,
    port: u16,
    username: String,
    expires_at: DateTime<Utc>,
}

pub(super) struct Paths {
    pub key: PathBuf,
    pub cert: PathBuf,
    pub known_hosts: PathBuf,
    pub meta: PathBuf,
}

pub(super) fn paths(dir: &Path, machine: &str) -> Paths {
    Paths {
        key: dir.join("id_ed25519"),
        cert: dir.join(format!("{machine}-cert.pub")),
        known_hosts: dir.join(format!("{machine}.known_hosts")),
        meta: dir.join(format!("{machine}.json")),
    }
}

/// The key pair, created on first use.
async fn ensure_key(dir: &Path) -> Result<String> {
    let key = dir.join("id_ed25519");
    let public = dir.join("id_ed25519.pub");
    if !public.exists() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let out = tokio::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "fleet", "-f"])
            .arg(&key)
            .output()
            .await
            .context("running ssh-keygen")?;
        if !out.status.success() {
            bail!(
                "ssh-keygen failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(std::fs::read_to_string(public)?.trim().to_string())
}

/// A valid certificate for `machine` (cached or fresh) and where to connect.
async fn connection(ib: &InstaBox, machine: &str) -> Result<(Paths, Cached)> {
    let p = paths(&ib.keys_dir, machine);
    if let Ok(text) = std::fs::read_to_string(&p.meta) {
        if let Ok(c) = serde_json::from_str::<Cached>(&text) {
            if c.expires_at > Utc::now() + ChronoDuration::minutes(5) && p.cert.exists() {
                return Ok((p, c));
            }
        }
    }
    let public = ensure_key(&ib.keys_dir).await?;
    let v = ib
        .api
        .call(
            Method::POST,
            &format!("/v1/machines/{machine}/connections"),
            Some(json!({ "publicKey": public, "ttlSeconds": CERT_TTL_SECS })),
            Opts::default(),
        )
        .await?;
    let host = v["host"]
        .as_str()
        .context("connection: no host")?
        .to_string();
    let port = v["port"].as_u64().context("connection: no port")? as u16;
    let username = v["username"]
        .as_str()
        .context("connection: no username")?
        .to_string();
    let cert = v["certificate"]
        .as_str()
        .context("connection: no certificate")?;
    let ca = v["hostTrust"]["publicKey"]
        .as_str()
        .context("connection: no host CA")?;
    let expires_at = v["expiresAt"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() + ChronoDuration::seconds(CERT_TTL_SECS as i64));
    std::fs::write(&p.cert, format!("{}\n", cert.trim()))?;
    std::fs::write(
        &p.known_hosts,
        format!("@cert-authority [{host}]:{port} {}\n", ca.trim()),
    )?;
    let c = Cached {
        host,
        port,
        username,
        expires_at,
    };
    std::fs::write(&p.meta, serde_json::to_string(&c)?)?;
    Ok((p, c))
}

/// `ssh` options that pin fleet's key, certificate and the gateway's CA.
fn ssh_opts(p: &Paths, port: u16) -> Vec<String> {
    vec![
        "-i".into(),
        p.key.display().to_string(),
        "-o".into(),
        format!("CertificateFile={}", p.cert.display()),
        "-o".into(),
        format!("UserKnownHostsFile={}", p.known_hosts.display()),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-p".into(),
        port.to_string(),
    ]
}

pub(super) async fn tty_command(
    ib: &InstaBox,
    machine: &str,
    argv: &[String],
) -> Result<Vec<String>> {
    let (p, c) = connection(ib, machine).await?;
    let target = format!("{}@{}", c.username, c.host);
    let remote = shell_words::join(argv);
    if ib.mosh {
        let ssh = shell_words::join(std::iter::once("ssh".to_string()).chain(ssh_opts(&p, c.port)));
        let mut cmd = vec!["mosh".into(), format!("--ssh={ssh}"), target, "--".into()];
        cmd.extend(argv.iter().cloned());
        return Ok(cmd);
    }
    let mut cmd = vec!["ssh".to_string(), "-t".into()];
    cmd.extend(ssh_opts(&p, c.port));
    cmd.push(target);
    if !argv.is_empty() {
        cmd.push("--".into());
        cmd.push(remote);
    }
    Ok(cmd)
}
