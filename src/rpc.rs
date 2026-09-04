use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WalletStatus {
    pub initialized: bool,
    pub header_height: Option<u32>,
    pub scanned_height: Option<u32>,
    pub sync_target: Option<u32>,
    pub syncing: bool,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Balance {
    pub total_zatoshis: u64,
    pub spendable_zatoshis: u64,
}

// Intentionally not Debug: RPC requests containing seeds must not be logged.
#[derive(Serialize, Deserialize)]
pub struct RestoreRequest {
    pub mnemonic: String,
    pub birthday: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CreatedWallet {
    pub mnemonic: String,
    pub address: String,
    pub birthday: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendRequest {
    pub address: String,
    pub amount_zatoshis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendResult {
    pub txid: String,
    pub submission: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionSummary {
    pub txid: String,
    pub mined_height: Option<u32>,
    pub expiry_height: u32,
}

#[rpc(server, client, namespace = "veil")]
pub trait WalletRpc {
    #[method(name = "status")]
    async fn status(&self) -> RpcResult<WalletStatus>;
    #[method(name = "create")]
    async fn create(&self) -> RpcResult<CreatedWallet>;
    #[method(name = "restore")]
    async fn restore(&self, request: RestoreRequest) -> RpcResult<String>;
    #[method(name = "address")]
    async fn address(&self) -> RpcResult<String>;
    #[method(name = "balance")]
    async fn balance(&self) -> RpcResult<Balance>;
    #[method(name = "sync")]
    async fn sync(&self) -> RpcResult<()>;
    #[method(name = "send")]
    async fn send(&self, request: SendRequest) -> RpcResult<SendResult>;
    #[method(name = "transactions")]
    async fn transactions(&self) -> RpcResult<Vec<TransactionSummary>>;
}
