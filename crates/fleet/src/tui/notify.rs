//! macOS banners on transitions *into* a waiting state, rate-limited per row.

use fleet_core::model::{Activity, Fleet, Overall};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const COOLDOWN: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct Notifier {
    prev: HashMap<String, (Overall, Vec<Activity>)>,
    last_sent: HashMap<String, Instant>,
    pub enabled: bool,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }

    /// Compare with the previous snapshot; fire banners for new NEEDS YOU / FAILED / READY rows.
    pub fn observe(&mut self, f: &Fleet) {
        let mut next = HashMap::new();
        for r in &f.overview {
            let key = r.dir.to_string_lossy().into_owned();
            let acts: Vec<Activity> = r.agents.iter().map(|a| a.activity).collect();
            if let Some((prev_overall, prev_acts)) = self.prev.get(&key) {
                let newly_waiting = acts.iter().any(|a| a.is_waiting())
                    && !prev_acts.iter().any(|a| a.is_waiting());
                let flipped = r.overall != *prev_overall
                    && matches!(r.overall, Overall::Failed | Overall::Ready);
                if (newly_waiting || flipped) && self.enabled {
                    self.send(&key, r);
                }
            }
            next.insert(key, (r.overall, acts));
        }
        self.prev = next;
    }

    fn send(&mut self, key: &str, r: &fleet_core::model::OverviewRow) {
        let now = Instant::now();
        if self
            .last_sent
            .get(key)
            .map(|t| now.duration_since(*t) < COOLDOWN)
            .unwrap_or(false)
        {
            return;
        }
        self.last_sent.insert(key.to_string(), now);
        let what = r
            .agents
            .iter()
            .find(|a| a.activity.is_waiting())
            .map(|a| a.activity.label().to_string())
            .unwrap_or_else(|| r.overall.label().to_string());
        let title = format!("fleet: {} · {what}", r.name());
        let body = r
            .agents
            .first()
            .and_then(|a| a.plan_title.clone().or(a.title.clone()))
            .unwrap_or_default();
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "'").chars().take(120).collect::<String>(),
            title.replace('"', "'")
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn();
    }
}
