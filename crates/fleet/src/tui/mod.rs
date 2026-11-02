pub mod app;
pub mod keys;
pub mod notify;
pub mod popups;
pub mod tabs;
pub mod util;

use anyhow::Result;
use fleet_core::Config;

pub async fn run(cfg: Config) -> Result<()> {
    app::run(cfg).await
}
