use crate::{
    config::Config,
    keys,
    network::node::{LightNode, network_parameters},
    rpc::*,
    sync::{self, ChainSource},
    wallet::WalletHandle,
};
use anyhow::Result;
use http::{Request, Response, header::AUTHORIZATION};
use jsonrpsee::{
    core::{RpcResult, async_trait},
    server::{HttpBody, ServerBuilder},
    types::ErrorObjectOwned,
};
use std::{
    future::Future,
    io::Write,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{Mutex, watch};
use tower::Service;
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Rpc {
    wallet: WalletHandle,
    source: ChainSource,
    gate: Arc<Mutex<()>>,
    progress: watch::Sender<(bool, Option<String>)>,
}
fn rpc_error(error: anyhow::Error) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32000, error.to_string(), None::<()>)
}
impl Rpc {
    async fn run_sync(&self) -> Result<()> {
        let _guard = self.gate.lock().await;
        self.progress.send_replace((true, None));
        let result = sync::sync(&self.wallet, &self.source).await;
        self.progress
            .send_replace((false, result.as_ref().err().map(ToString::to_string)));
        result
    }
}
#[async_trait]
impl WalletRpcServer for Rpc {
    async fn status(&self) -> RpcResult<WalletStatus> {
        let mut status = self.wallet.call(|w| w.status()).await.map_err(rpc_error)?;
        let (syncing, last_error) = self.progress.borrow().clone();
        status.syncing = syncing;
        status.last_error = last_error;
        status.header_height = self.source.header_height();
        Ok(status)
    }
    async fn create(&self) -> RpcResult<CreatedWallet> {
        let _guard = self.gate.lock().await;
        let height = self.source.target().await.map_err(rpc_error)?;
        let state = self.source.tree_state(height).await.map_err(rpc_error)?;
        let mnemonic = keys::generate_mnemonic();
        let returned = mnemonic.to_string();
        let address = self
            .wallet
            .call(move |w| w.initialize(&mnemonic, state))
            .await
            .map_err(rpc_error)?;
        Ok(CreatedWallet {
            mnemonic: returned,
            address,
            birthday: height + 1,
        })
    }
    async fn restore(&self, request: RestoreRequest) -> RpcResult<String> {
        let mnemonic = Zeroizing::new(request.mnemonic);
        let _guard = self.gate.lock().await;
        let height = request
            .birthday
            .checked_sub(1)
            .ok_or_else(|| rpc_error(anyhow::anyhow!("birthday must be positive")))?;
        let state = self.source.tree_state(height).await.map_err(rpc_error)?;
        self.wallet
            .call(move |w| w.initialize(&mnemonic, state))
            .await
            .map_err(rpc_error)
    }
    async fn address(&self) -> RpcResult<String> {
        self.wallet.call(|w| w.address()).await.map_err(rpc_error)
    }
    async fn balance(&self) -> RpcResult<Balance> {
        self.wallet.call(|w| w.balance()).await.map_err(rpc_error)
    }
    async fn transactions(&self) -> RpcResult<Vec<TransactionSummary>> {
        self.wallet
            .call(|w| w.transactions())
            .await
            .map_err(rpc_error)
    }
    async fn sync(&self) -> RpcResult<()> {
        self.run_sync().await.map_err(rpc_error)
    }
    async fn send(&self, request: SendRequest) -> RpcResult<SendResult> {
        self.run_sync().await.map_err(rpc_error)?;
        let _guard = self.gate.lock().await;
        self.wallet
            .call(move |w| w.send(request))
            .await
            .map_err(rpc_error)
    }
}

pub async fn run(config: Config) -> Result<()> {
    config.validate()?;
    let wallet = WalletHandle::open(config.clone())?;
    let node = LightNode::start(&config).await?;
    let result = serve(&config, wallet, &node).await;
    let stopped = node.shutdown().await;
    stopped.and(result)
}
async fn serve(config: &Config, wallet: WalletHandle, node: &LightNode) -> Result<()> {
    let source = ChainSource::new(
        node.compact.clone(),
        node.services.read_state.clone(),
        network_parameters(config.network)?,
    )?;
    let (progress, _) = watch::channel((false, None));
    let rpc = Rpc {
        wallet,
        source,
        gate: Arc::new(Mutex::new(())),
        progress,
    };
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let cookie_temp = config
        .data_dir
        .join(format!("rpc-cookie-{}.tmp", uuid::Uuid::new_v4()));
    let mut cookie = keys::private_file(&cookie_temp)?;
    cookie.write_all(token.as_bytes())?;
    cookie.sync_all()?;
    std::fs::rename(cookie_temp, config.cookie_path())?;
    let auth = http::HeaderValue::from_str(&format!("Bearer {token}"))?;
    let middleware = tower::ServiceBuilder::new().layer_fn(move |inner| CookieAuth {
        inner,
        auth: auth.clone(),
    });
    let server = ServerBuilder::default()
        .set_config(
            jsonrpsee::server::ServerConfig::builder()
                .http_only()
                .max_request_body_size(16 * 1024)
                .max_connections(16)
                .build(),
        )
        .set_http_middleware(middleware)
        .build(config.rpc_listen)
        .await?;
    let handle = server.start(rpc.clone().into_rpc());
    tracing::info!(listen = %config.rpc_listen, "veild is ready");
    let mut interval = tokio::time::interval(Duration::from_secs(config.sync_interval_seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Ok(()),
            _ = handle.clone().stopped() => break Ok(()),
            _ = interval.tick() => {
                if node.is_finished() {
                    break Err(anyhow::anyhow!("embedded header node stopped"));
                }
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break Ok(()),
                    result = rpc.run_sync() => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "wallet sync paused");
                        }
                    }
                }
            }
        }
    };
    let _ = handle.stop();
    handle.stopped().await;
    std::fs::remove_file(config.cookie_path())?;
    result
}

#[derive(Clone)]
struct CookieAuth<S> {
    inner: S,
    auth: http::HeaderValue,
}
impl<S, B> Service<Request<B>> for CookieAuth<S>
where
    S: Service<Request<B>, Response = Response<HttpBody>>,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
{
    type Response = Response<HttpBody>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }
    fn call(&mut self, request: Request<B>) -> Self::Future {
        if request.headers().get(AUTHORIZATION) != Some(&self.auth) {
            return Box::pin(async {
                Ok(Response::builder()
                    .status(401)
                    .body(HttpBody::from("unauthorized"))
                    .expect("constant HTTP response is valid"))
            });
        }
        let future = self.inner.call(request);
        Box::pin(async move { future.await.map_err(Into::into) })
    }
}
