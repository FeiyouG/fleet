//! Machines: the computers agents run on.
//!
//! `interfaces/` is what the rest of fleet depends on: the `MachineAdapter` trait and the
//! machine types. `adapters/` holds one implementation per provider (`local`, `instabox`).
//! Nothing outside this module names a provider directly: `adapter_for(cfg)` builds one from
//! a resolved machine profile. Only built-in adapters exist (spec: machines).

pub mod adapters;
pub mod interfaces;
pub mod remote;
pub mod state;

pub use adapters::{adapter_for, instabox, local, PROVIDERS};
pub use interfaces::*;
