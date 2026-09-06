use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use jsonrpsee::http_client::HttpClientBuilder;
use std::{io::Read, path::PathBuf, time::Duration};
use veil::{
    config::Config,
    rpc::{RestoreRequest, SendRequest, WalletRpcClient},
};

#[derive(Parser)]
#[command(about = "A minimal Ironwood wallet")]
struct Cli {
    #[arg(long, default_value = "veil.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Status,
    /// Create a wallet. The JSON response contains its recovery phrase.
    Create,
    /// Restore from a phrase file; use '-' to read redirected standard input.
    Restore {
        #[arg(long)]
        mnemonic_file: PathBuf,
        #[arg(long)]
        birthday: u32,
    },
    Address,
    Balance,
    Sync,
    /// Build a transaction and capture it locally (mock submission).
    Send {
        address: String,
        #[arg(long)]
        amount_zatoshis: u64,
    },
    Transactions,
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    let cookie = zeroize::Zeroizing::new(
        std::fs::read_to_string(config.cookie_path())
            .context("read RPC cookie; start veild first")?,
    );
    let mut headers = http::HeaderMap::new();
    let mut auth = http::HeaderValue::from_str(&format!("Bearer {}", cookie.trim()))?;
    auth.set_sensitive(true);
    headers.insert(http::header::AUTHORIZATION, auth);
    let client = HttpClientBuilder::default()
        .set_headers(headers)
        .request_timeout(Duration::from_secs(1200))
        .build(format!("http://{}", config.rpc_listen))?;
    let value = match cli.command {
        Command::Status => serde_json::to_value(client.status().await?)?,
        Command::Create => serde_json::to_value(client.create().await?)?,
        Command::Restore {
            mnemonic_file,
            birthday,
        } => {
            let mut mnemonic = zeroize::Zeroizing::new(String::new());
            if mnemonic_file.as_os_str() == "-" {
                std::io::stdin().take(4096).read_to_string(&mut mnemonic)?;
            } else {
                std::fs::File::open(mnemonic_file)?
                    .take(4096)
                    .read_to_string(&mut mnemonic)?;
            }
            serde_json::to_value(
                client
                    .restore(RestoreRequest {
                        mnemonic: mnemonic.trim().to_owned(),
                        birthday,
                    })
                    .await?,
            )?
        }
        Command::Address => serde_json::to_value(client.address().await?)?,
        Command::Balance => serde_json::to_value(client.balance().await?)?,
        Command::Sync => {
            client.sync().await?;
            serde_json::Value::Null
        }
        Command::Send {
            address,
            amount_zatoshis,
        } => serde_json::to_value(
            client
                .send(SendRequest {
                    address,
                    amount_zatoshis,
                })
                .await?,
        )?,
        Command::Transactions => serde_json::to_value(client.transactions().await?)?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
