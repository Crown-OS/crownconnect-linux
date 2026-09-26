mod app;

use anyhow::{Context, Result};
use crownconnect_linux::config::DaemonConfig;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const LOG_VARIABLE: &str = "CROWNCONNECT_LOG";
const DEFAULT_LOG: &str = "info";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_logging()?;
    let config = DaemonConfig::from_environment().context("cannot configure the daemon")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), name = %config.device_name, "crownconnect starting");
    app::run(config).await
}

fn init_logging() -> Result<()> {
    let filter: Targets = std::env::var(LOG_VARIABLE)
        .as_deref()
        .unwrap_or(DEFAULT_LOG)
        .parse()
        .with_context(|| format!("{LOG_VARIABLE} is not a valid filter"))?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .try_init()
        .context("cannot install the logger")
}
