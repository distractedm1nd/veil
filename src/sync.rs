use crate::{
    network::{compact::CompactClient, verify::RootVerifier},
    wallet::WalletHandle,
};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};
use zakura_state::ReadStateService;
use zcash_client_backend::{
    data_api::{
        TransactionDataRequest, TransactionStatus, WalletRead, WalletWrite,
        chain::{self, BlockSource, ChainState},
        wallet::decrypt_and_store_transaction,
    },
    proto::{
        compact_formats::CompactBlock,
        service::{BlockId, BlockRange, ChainSpec, RawTransaction, TreeState, TxFilter},
    },
};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId};
use ztreamer_protocol::p2p::Message;

#[derive(Clone)]
pub struct ChainSource {
    pub compact: CompactClient,
    state: ReadStateService,
    verifier: Arc<Mutex<RootVerifier>>,
}
impl ChainSource {
    pub fn header_height(&self) -> Option<u32> {
        self.state
            .subscribe_header_chain_snapshots()
            .borrow()
            .as_ref()
            .map(|s| s.frontiers.header_best.height.0)
    }
    pub fn new(
        compact: CompactClient,
        state: ReadStateService,
        network: zakura_chain::parameters::Network,
    ) -> Result<Self> {
        Ok(Self {
            compact,
            state,
            verifier: Arc::new(Mutex::new(RootVerifier::new(network)?)),
        })
    }
    pub async fn target(&self) -> Result<u32> {
        let latest: BlockId = self
            .compact
            .unary(Message::GetLatestBlockRequest, ChainSpec {})
            .await?;
        let header = self
            .state
            .subscribe_header_chain_snapshots()
            .borrow()
            .as_ref()
            .map(|s| s.frontiers.header_best.height.0)
            .context("header node is starting")?;
        // A successor header is required to authenticate a block's note-tree roots.
        Ok(u32::try_from(latest.height)?.min(header.saturating_sub(1)))
    }
    pub async fn tree_state(&self, height: u32) -> Result<ChainState> {
        let tree: TreeState = self
            .compact
            .unary(Message::GetTreeStateRequest, block_id(height))
            .await?;
        ensure!(
            tree.height == u64::from(height),
            "provider returned the wrong tree height"
        );
        let source = self.clone();
        tokio::task::spawn_blocking(move || {
            source
                .verifier
                .lock()
                .map_err(|_| anyhow!("root verifier panicked"))?
                .tree_state(&source.state, &tree)
        })
        .await?
    }
    async fn blocks(&self, start: u32, end: u32, from: ChainState) -> Result<Vec<CompactBlock>> {
        let blocks: Vec<CompactBlock> = self
            .compact
            .request(
                Message::GetBlockRangeRequest,
                BlockRange {
                    start: Some(block_id(start)),
                    end: Some(block_id(end)),
                    pool_types: vec![],
                },
                usize::try_from(end - start + 1)?,
            )
            .await?;
        ensure!(
            blocks.len() == usize::try_from(end - start + 1)?,
            "provider returned an incomplete block range"
        );
        let source = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut verifier = source
                .verifier
                .lock()
                .map_err(|_| anyhow!("root verifier panicked"))?;
            let mut sapling = from.final_sapling_tree().clone();
            let mut orchard = from.final_orchard_tree().clone();
            let mut ironwood = from.final_ironwood_tree().clone();
            let mut previous_hash = from.block_hash().0;
            for (index, block) in blocks.iter().enumerate() {
                let height = start
                    .checked_add(u32::try_from(index)?)
                    .context("height overflow")?;
                ensure!(
                    block.height == u64::from(height)
                        && block.hash.len() == 32
                        && block.prev_hash.len() == 32,
                    "invalid compact block identity"
                );
                let (hash, roots) = verifier.roots(&source.state, height)?;
                ensure!(
                    block.hash == hash.0 && block.prev_hash == previous_hash,
                    "compact block is not on the selected header chain"
                );
                for tx in &block.vtx {
                    ensure!(tx.txid.len() == 32, "invalid compact transaction ID");
                    for output in &tx.outputs {
                        ensure!(
                            sapling.append(sapling::Node::from_cmu(
                                &output
                                    .cmu()
                                    .map_err(|e| anyhow!("invalid commitment: {e:?}"))?
                            )),
                            "Sapling tree is full"
                        );
                    }
                    for action in &tx.actions {
                        ensure!(
                            orchard.append(orchard::tree::MerkleHashOrchard::from_cmx(
                                &action
                                    .cmx()
                                    .map_err(|e| anyhow!("invalid commitment: {e:?}"))?
                            )),
                            "Orchard tree is full"
                        );
                    }
                    for action in &tx.ironwood_actions {
                        ensure!(
                            ironwood.append(orchard::tree::MerkleHashOrchard::from_cmx(
                                &action
                                    .cmx()
                                    .map_err(|e| anyhow!("invalid commitment: {e:?}"))?
                            )),
                            "Ironwood tree is full"
                        );
                    }
                }
                ensure!(
                    sapling.root().to_bytes() == <[u8; 32]>::from(roots.sapling_root),
                    "compact Sapling commitments do not match the header"
                );
                ensure!(
                    orchard.root().to_bytes() == <[u8; 32]>::from(roots.orchard_root),
                    "compact Orchard commitments do not match the header"
                );
                ensure!(
                    ironwood.root().to_bytes() == <[u8; 32]>::from(roots.ironwood_root),
                    "compact Ironwood commitments do not match the header"
                );
                previous_hash = hash.0;
            }
            Ok(blocks)
        })
        .await?
    }
}
fn block_id(height: u32) -> BlockId {
    BlockId {
        height: u64::from(height),
        hash: vec![],
    }
}

pub async fn sync(wallet: &WalletHandle, source: &ChainSource) -> Result<()> {
    if !wallet.call(|w| w.initialized()).await? {
        return Ok(());
    }
    let target = source.target().await?;
    // Find a common ancestor before applying new scan results. Database truncation uses
    // the library's retained checkpoints; a deeper reorg reports an error requiring restore.
    loop {
        let scanned = wallet.call(|w| Ok(w.db.block_fully_scanned()?)).await?;
        let Some(scanned) = scanned else {
            break;
        };
        let height = u32::from(scanned.block_height());
        // A lagging provider (or a header node still starting) is not a reorg.
        ensure!(height <= target, "waiting for the chain source to catch up");
        let chain = source.tree_state(height).await?;
        let matches = chain.block_hash() == scanned.block_hash();
        if matches {
            break;
        }
        ensure!(height > 0, "reorg reached genesis");
        wallet
            .call(move |w| {
                w.db.truncate_to_height(BlockHeight::from_u32(height - 1))?;
                Ok(())
            })
            .await?;
    }
    wallet
        .call(move |w| {
            w.db.update_chain_tip(target.into())?;
            Ok(())
        })
        .await?;
    loop {
        let range = wallet
            .call(|w| Ok(w.db.suggest_scan_ranges()?.into_iter().next()))
            .await?;
        let Some(range) = range else {
            break;
        };
        let start = u32::from(range.block_range().start);
        let end = u32::from(range.block_range().end)
            .saturating_sub(1)
            .min(start.saturating_add(99))
            .min(target);
        if start > end {
            break;
        }
        let from = source
            .tree_state(start.checked_sub(1).context("cannot scan genesis")?)
            .await?;
        let blocks = source.blocks(start, end, from.clone()).await?;
        wallet
            .call(move |w| {
                chain::scan_cached_blocks(
                    &w.config.network,
                    &Blocks(blocks),
                    &mut w.db,
                    start.into(),
                    &from,
                    usize::try_from(end - start + 1)?,
                )?;
                Ok(())
            })
            .await?;
    }
    enhance(wallet, source).await
}

async fn enhance(wallet: &WalletHandle, source: &ChainSource) -> Result<()> {
    let requests = wallet
        .call(|w| Ok(w.db.transaction_data_requests()?))
        .await?;
    for request in requests {
        let (TransactionDataRequest::GetStatus(txid) | TransactionDataRequest::Enhancement(txid)) =
            request;
        let response: Result<RawTransaction> = source
            .compact
            .unary(
                Message::GetTransactionRequest,
                TxFilter {
                    hash: txid.as_ref().to_vec(),
                    ..Default::default()
                },
            )
            .await;
        let raw = match response {
            Ok(raw) => raw,
            Err(error)
                if error
                    .downcast_ref::<crate::network::compact::ProviderError>()
                    .is_some_and(|e| e.0.code == 5) =>
            {
                wallet
                    .call(move |w| {
                        w.db.set_transaction_status(txid, TransactionStatus::TxidNotRecognized)?;
                        Ok(())
                    })
                    .await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        wallet
            .call(move |w| {
                let height = match raw.height {
                    0 | u64::MAX => None,
                    h => Some(BlockHeight::from_u32(u32::try_from(h)?)),
                };
                let branch_height = height
                    .or(w.db.chain_height()?)
                    .context("chain height is unknown")?;
                let tx = Transaction::read(
                    raw.data.as_slice(),
                    BranchId::for_height(&w.config.network, branch_height),
                )?;
                ensure!(tx.txid() == txid, "provider returned the wrong transaction");
                decrypt_and_store_transaction(&w.config.network, &mut w.db, &tx, height)?;
                w.db.set_transaction_status(
                    txid,
                    height
                        .map(TransactionStatus::Mined)
                        .unwrap_or(TransactionStatus::NotInMainChain),
                )?;
                Ok(())
            })
            .await?;
    }
    Ok(())
}
struct Blocks(Vec<CompactBlock>);
impl BlockSource for Blocks {
    type Error = Infallible;
    fn with_blocks<F, E>(
        &self,
        from: Option<BlockHeight>,
        limit: Option<usize>,
        mut consume: F,
    ) -> Result<(), chain::error::Error<E, Infallible>>
    where
        F: FnMut(CompactBlock) -> Result<(), chain::error::Error<E, Infallible>>,
    {
        for block in self
            .0
            .iter()
            .filter(|block| from.is_none_or(|from| block.height >= u64::from(u32::from(from))))
            .take(limit.unwrap_or(usize::MAX))
        {
            consume(block.clone())?;
        }
        Ok(())
    }
}
