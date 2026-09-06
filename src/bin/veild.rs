use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Veil wallet daemon")]
struct Cli {
    #[arg(long, default_value = "veil.toml")]
    config: PathBuf,
}
#[cfg(feature = "node")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    veil::daemon::run(veil::config::Config::load(&cli.config)?).await
}
#[cfg(not(feature = "node"))]
fn main() {
    eprintln!("veild requires the node feature");
    std::process::exit(1);
}
