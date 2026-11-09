//! Coding-agent integrations.
//!
//! `interfaces/` is what the rest of fleet depends on: the `AgentAdapter` trait, the launch
//! and send types, the agent-neutral state file every adapter writes, and the hook event
//! model. `adapters/` holds one implementation per harness (opencode, claude, codex). Nothing
//! outside this module names a harness directly: `adapters(cfg)` builds the set from config.

pub mod adapters;
pub mod interfaces;

pub use adapters::{claude, codex, hooked, opencode};
pub use interfaces::*;
