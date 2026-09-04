use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use zcash_protocol::{
    consensus::{BlockHeight, Network, NetworkType, NetworkUpgrade, Parameters},
    local_consensus::LocalNetwork,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub data_dir: PathBuf,
    pub network: WalletNetwork,
    pub rpc_listen: SocketAddr,
    pub peers: Vec<String>,
    pub sync_interval_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            network: WalletNetwork::Mainnet,
            rpc_listen: "127.0.0.1:18237"
                .parse()
                .expect("constant loopback address"),
            peers: Vec::new(),
            sync_interval_seconds: 10,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self = toml::from_str(
            &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
        )?;
        if config.data_dir.is_relative() {
            config.data_dir = path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&config.data_dir);
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.rpc_listen.ip().is_loopback(),
            "veil RPC must bind loopback"
        );
        ensure!(
            self.sync_interval_seconds > 0,
            "sync interval must be positive"
        );
        if let WalletNetwork::Regtest {
            ironwood_activation,
        } = self.network
        {
            ensure!(
                ironwood_activation >= 2,
                "regtest Ironwood activation must be at least 2"
            );
        }
        Ok(())
    }

    pub fn wallet_path(&self) -> PathBuf {
        self.data_dir.join("wallet.db")
    }
    pub fn identity_path(&self) -> PathBuf {
        self.data_dir.join("encryption-identity.txt")
    }
    pub fn cookie_path(&self) -> PathBuf {
        self.data_dir.join("rpc.cookie")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum WalletNetwork {
    Mainnet,
    Regtest { ironwood_activation: u32 },
}

impl WalletNetwork {
    fn local(self) -> LocalNetwork {
        let one = Some(BlockHeight::from_u32(1));
        let Self::Regtest {
            ironwood_activation,
        } = self
        else {
            unreachable!("only called for regtest")
        };
        LocalNetwork {
            overwinter: one,
            sapling: one,
            blossom: one,
            heartwood: one,
            canopy: one,
            nu5: one,
            nu6: one,
            nu6_1: one,
            nu6_2: one,
            nu6_3: Some(ironwood_activation.into()),
        }
    }
}

impl Parameters for WalletNetwork {
    fn network_type(&self) -> NetworkType {
        match self {
            Self::Mainnet => NetworkType::Main,
            Self::Regtest { .. } => NetworkType::Regtest,
        }
    }
    fn activation_height(&self, upgrade: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Self::Mainnet => Network::MainNetwork.activation_height(upgrade),
            Self::Regtest { .. } => self.local().activation_height(upgrade),
        }
    }
}
