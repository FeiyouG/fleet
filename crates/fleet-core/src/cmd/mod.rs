//! Commands that change the world: start, rename, review, send, close, gc.

pub mod agent_hook;
pub mod close;
pub mod gc;
pub mod machine;
pub mod project;
pub mod prompt;
pub mod rename;
pub mod review;
pub mod send;
pub mod setup;
pub mod start;
pub mod txn;

use anyhow::{Context, Result};
use std::net::TcpListener;

/// A free loopback port for an agent's `--port`.
pub fn free_port() -> Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0").context("binding a free port")?;
    Ok(l.local_addr()?.port())
}

/// Lines that go to stdout as a command progresses.
pub type Progress = Box<dyn Fn(&str) + Send + Sync>;

pub fn quiet() -> Progress {
    Box::new(|_| {})
}
