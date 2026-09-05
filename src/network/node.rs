use super::compact::CompactClient;
use crate::config::{Config, WalletNetwork};
use anyhow::{Context, Result, anyhow};
use std::{sync::Arc, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use zakura_chain::parameters::{
    Network,
    testnet::{ConfiguredActivationHeights, Parameters},
};
use zakura_state::{ReadRequest, ReadResponse};

pub struct LightNode {
    pub services: zakurad::node::NodeServices,
    pub compact: CompactClient,
    shutdown: CancellationToken,
    task: JoinHandle<Result<()>>,
}
impl LightNode {
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub async fn start(config: &Config) -> Result<Self> {
        let mut node = zakurad::config::ZakuradConfig::default();
        node.network.network = network_parameters(config.network)?;
        node.state.cache_dir = config.data_dir.join("node/state");
        node.state.headers_only = true;
        node.network.identity_dir = config.data_dir.join("node/identity");
        node.network.cache_dir = zakura_network::CacheDir::disabled();
        node.network.initial_mainnet_peers.clear();
        node.network.initial_testnet_peers.clear();
        node.network.zakura.listen_addr = None;
        node.network.zakura.bootstrap_peers = config.peers.clone();
        node.rpc.listen_addr = None;
        let (compact, service) = CompactClient::service();
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let (ready, receiver) = oneshot::channel();
        let runtime = tokio::runtime::Handle::current();
        let task = tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                zakurad::node::run_with_services_ready(node, vec![service], stop, ready)
                    .await
                    .map_err(|e| anyhow!("embedded node: {e:#}"))
            })
        });
        let services = match tokio::time::timeout(Duration::from_secs(120), receiver).await {
            Ok(Ok(services)) => services,
            _ => {
                shutdown.cancel();
                if task.is_finished() {
                    return Err(task
                        .await?
                        .err()
                        .unwrap_or_else(|| anyhow!("embedded node exited before becoming ready")));
                }
                task.abort();
                return Err(anyhow!("embedded node did not become ready"));
            }
        };
        Ok(Self {
            services,
            compact,
            shutdown,
            task,
        })
    }
    pub async fn header_height(&self) -> Result<Option<u32>> {
        match tokio::time::timeout(
            Duration::from_secs(10),
            self.services
                .read_state
                .clone()
                .oneshot(ReadRequest::HeaderChainSnapshot),
        )
        .await
        .context("header state timed out")?
        .map_err(|e| anyhow!("header state: {e}"))?
        {
            ReadResponse::HeaderChainSnapshot(snapshot) => {
                Ok(snapshot.map(|s| s.frontiers.header_best.height.0))
            }
            _ => Err(anyhow!("unexpected header state response")),
        }
    }
    pub async fn shutdown(self) -> Result<()> {
        let Self {
            services,
            compact,
            shutdown,
            task,
        } = self;
        shutdown.cancel();
        drop(services);
        drop(compact);
        tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .context("node shutdown timed out")???;
        Ok(())
    }
}

pub fn network_parameters(network: WalletNetwork) -> Result<Network> {
    Ok(match network {
        WalletNetwork::Mainnet => Network::Mainnet,
        WalletNetwork::Regtest {
            ironwood_activation,
        } => Network::Testnet(Arc::new(
            Parameters::new_regtest(
                ConfiguredActivationHeights {
                    overwinter: Some(1),
                    sapling: Some(1),
                    blossom: Some(1),
                    heartwood: Some(1),
                    canopy: Some(1),
                    nu5: Some(1),
                    nu6: Some(1),
                    nu6_1: Some(1),
                    nu6_2: Some(1),
                    nu6_3: Some(ironwood_activation),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(|e| anyhow!("invalid regtest parameters: {e}"))?,
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcash_protocol::consensus::{NetworkUpgrade, Parameters};

    #[test]
    fn wallet_and_header_node_agree_on_upgrade_heights() {
        for wallet in [
            WalletNetwork::Mainnet,
            WalletNetwork::Regtest {
                ironwood_activation: 2,
            },
        ] {
            let node = network_parameters(wallet).unwrap();
            for upgrade in [
                NetworkUpgrade::Overwinter,
                NetworkUpgrade::Sapling,
                NetworkUpgrade::Blossom,
                NetworkUpgrade::Heartwood,
                NetworkUpgrade::Canopy,
                NetworkUpgrade::Nu5,
                NetworkUpgrade::Nu6,
                NetworkUpgrade::Nu6_1,
                NetworkUpgrade::Nu6_2,
                NetworkUpgrade::Nu6_3,
            ] {
                assert_eq!(
                    wallet.activation_height(upgrade).map(u32::from),
                    zakura_chain::parameters::NetworkUpgrade::from(upgrade)
                        .activation_height(&node)
                        .map(|height| height.0),
                    "{wallet:?}: {upgrade:?}",
                );
            }
        }
    }
}
