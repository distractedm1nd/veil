use veil::{
    config::{Config, WalletNetwork},
    network::node::LightNode,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let data_dir = std::env::args()
        .nth(1)
        .map(Into::into)
        .ok_or_else(|| anyhow::anyhow!("usage: node_smoke DATA_DIR"))?;
    let config = Config {
        data_dir,
        network: WalletNetwork::Regtest {
            ironwood_activation: 2,
        },
        ..Default::default()
    };
    let node = LightNode::start(&config).await?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while node.header_height().await?.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    anyhow::ensure!(
        node.header_height().await? == Some(0),
        "unexpected header tip"
    );
    anyhow::ensure!(
        node.services
            .read_state
            .best_tip()
            .map(|(height, _)| height.0)
            == Some(0),
        "block store should contain only genesis"
    );
    node.shutdown().await?;
    Ok(())
}
