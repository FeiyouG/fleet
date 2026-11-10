//! One adapter per provider. Only these exist: a machine profile naming another provider
//! is a config error (`config::validate`).

pub mod instabox;
pub mod local;

use super::interfaces::MachineAdapter;
use crate::config::MachineConfig;
use anyhow::{bail, Result};

/// Every provider fleet ships.
pub const PROVIDERS: &[&str] = &["local", "instabox"];

/// The adapter for a resolved machine profile.
pub fn adapter_for(cfg: &MachineConfig) -> Result<Box<dyn MachineAdapter>> {
    match cfg.provider.as_str() {
        "local" => Ok(Box::new(local::Local::new(&cfg.name))),
        "instabox" => Ok(Box::new(instabox::InstaBox::from_config(cfg)?)),
        p => bail!(
            "machines.{}: unknown provider \"{p}\" (known: {})",
            cfg.name,
            PROVIDERS.join(", ")
        ),
    }
}
