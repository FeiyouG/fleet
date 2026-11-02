pub mod keys;
use anyhow::Result;
use fleet_core::Config;
pub async fn run(_cfg: Config) -> Result<()> {
    anyhow::bail!("tui not built yet")
}
