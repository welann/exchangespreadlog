use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use exchangespreadlog::{app::runner::AppRunner, config::Config, telemetry};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Collect top-of-book spreads from perpetual DEX venues"
)]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    #[arg(long)]
    no_tui: bool,

    #[arg(long)]
    print_default_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init();
    install_crypto_provider();

    let args = Args::parse();
    if args.print_default_config {
        println!("{}", Config::default_toml()?);
        return Ok(());
    }

    let mut config = Config::load_or_default(&args.config)?;
    if args.no_tui {
        config.tui.enabled = false;
    }
    AppRunner::new(config).run().await
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
