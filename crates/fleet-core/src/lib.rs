//! fleet-core: collectors, joiner, status rules and commands behind the `fleet` TUI.
//! No terminal code lives here; every type in [`model`] is `Serialize`, so any frontend
//! (the ratatui TUI, `fleet status --json`, a future `fleet serve`) renders the same data.

pub mod agents;
pub mod cmd;
pub mod config;
pub mod join;
pub mod model;
pub mod procs;
pub mod registry;
pub mod reviews;
pub mod slug;
pub mod snapshot;
pub mod status;
pub mod tmux;
pub mod worktrees;

pub use config::Config;
pub use model::Fleet;
pub use snapshot::Engine;
