//! Plannotator probe: the plan-review server runs *inside* the opencode process on a random
//! loopback port. Listing the process's listening ports and asking `GET /api/plan` tells us
//! both that a plan is waiting and where its tab lives.

use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::process::Command;

/// pid -> listening TCP ports on loopback, for the given pids. Uses `lsof` (macOS has no
/// portable /proc); one call for all pids.
pub async fn listening_ports(pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    let mut map = HashMap::new();
    if pids.is_empty() {
        return map;
    }
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let out = Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-iTCP",
            "-sTCP:LISTEN",
            "-p",
            &pid_list,
            "-F",
            "pn",
        ])
        .output()
        .await;
    let Ok(out) = out else { return map };
    parse_lsof(&String::from_utf8_lossy(&out.stdout), &mut map);
    map
}

/// `-F pn` output: `p<pid>` lines followed by `n<addr>:<port>` lines.
pub fn parse_lsof(s: &str, map: &mut HashMap<u32, Vec<u16>>) {
    let mut cur: Option<u32> = None;
    for line in s.lines() {
        if let Some(p) = line.strip_prefix('p') {
            cur = p.parse().ok();
        } else if let (Some(pid), Some(n)) = (cur, line.strip_prefix('n')) {
            if let Some(port) = n.rsplit(':').next().and_then(|x| x.parse::<u16>().ok()) {
                let v = map.entry(pid).or_default();
                if !v.contains(&port) {
                    v.push(port);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub url: String,
    pub title: Option<String>,
}

/// Ask a port whether it is a Plannotator plan-review server. Returns the plan's first
/// heading when it is.
pub async fn probe(port: u16) -> Option<Plan> {
    let c = reqwest::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
        .ok()?;
    let url = format!("http://127.0.0.1:{port}");
    let r = c.get(format!("{url}/api/plan")).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: Value = r.json().await.ok()?;
    let plan = v.get("plan").and_then(Value::as_str)?;
    let title = plan
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string());
    Some(Plan { url, title })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsof_parse() {
        let s = "p12050\nf44\nn127.0.0.1:58759\np14510\nf40\nn127.0.0.1:61801\nn*:4096\n";
        let mut m = HashMap::new();
        parse_lsof(s, &mut m);
        assert_eq!(m[&12050], vec![58759]);
        assert_eq!(m[&14510], vec![61801, 4096]);
    }
}
