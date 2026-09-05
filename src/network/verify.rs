//! Authenticate note-tree roots against the locally selected header chain.
use anyhow::{Context, Result, anyhow, ensure};
use std::{collections::BTreeMap, sync::Arc};
use zakura_chain::{
    block::{Hash, Header, Height},
    history_tree::{HistoryTree, HistoryTreeBlockParts},
    parallel::{
        commitment_aux::BlockCommitmentRoots,
        commitment_aux_verify::{
            verify_supplied_ironwood_root_below_nu6_3, verify_supplied_ironwood_tx_below_nu6_3,
            verify_supplied_roots_from_parts,
        },
    },
    parameters::{Network, NetworkUpgrade},
};
use zakura_state::ReadStateService;
use zcash_client_backend::{data_api::chain::ChainState, proto::service::TreeState};

type Data = (Arc<Header>, BlockCommitmentRoots);
pub struct RootVerifier {
    network: Network,
    activation: u32,
    start: u32,
    pending: Option<(Data, HistoryTree)>,
    verified: BTreeMap<u32, (Hash, BlockCommitmentRoots)>,
}
impl RootVerifier {
    pub fn new(network: Network) -> Result<Self> {
        let activation = NetworkUpgrade::Nu6_3
            .activation_height(&network)
            .context("Ironwood is not activated on this network")?
            .0;
        Ok(Self {
            network,
            activation,
            start: u32::MAX,
            pending: None,
            verified: BTreeMap::new(),
        })
    }
    fn read(state: &ReadStateService, height: u32) -> Result<Data> {
        state
            .light_commitment_data(Height(height))
            .map_err(|e| anyhow!("read header commitments: {e}"))?
            .with_context(|| format!("header commitments at {height} are not available yet"))
    }
    pub fn roots(
        &mut self,
        state: &ReadStateService,
        height: u32,
    ) -> Result<(Hash, BlockCommitmentRoots)> {
        ensure!(
            height >= self.activation.saturating_sub(1),
            "wallet birthday precedes Ironwood"
        );
        if height < self.start {
            self.start =
                NetworkUpgrade::current_with_activation_height(&self.network, Height(height))
                    .1
                    .0;
            self.pending = None;
            self.verified.clear();
        }
        if let Some(((header, roots), _)) = &self.pending {
            let current = Self::read(state, roots.height.0)?;
            if current.0.hash() != header.hash() {
                self.pending = None;
                self.verified.clear();
            }
        }
        if let Some(roots) = self.verified.get(&height) {
            return Ok(roots.clone());
        }
        if self.pending.is_none() {
            let data = Self::read(state, self.start)?;
            // A pre-Ironwood history leaf does not bind the inactive pool fields.
            // Pin them to their consensus empties before accepting an activation birthday.
            verify_supplied_ironwood_root_below_nu6_3(
                &self.network,
                data.1.height,
                &data.1.ironwood_root,
            )?;
            verify_supplied_ironwood_tx_below_nu6_3(
                &self.network,
                data.1.height,
                data.1.ironwood_tx,
            )?;
            // ZIP-221 resets at each upgrade. Seeding with the activation leaf does not
            // authenticate it; only the successor's header check below does that.
            let tree = HistoryTree::from_parts(&self.network, parts(&data))?;
            self.pending = Some((data, tree));
        }
        loop {
            let (pending, history) = self.pending.as_ref().expect("history was initialized");
            let successor_height = pending
                .1
                .height
                .0
                .checked_add(1)
                .context("height overflow")?;
            let successor = Self::read(state, successor_height)?;
            ensure!(
                successor.0.previous_block_hash == pending.0.hash(),
                "selected header branch changed; retry sync"
            );
            verify_supplied_roots_from_parts(
                &self.network,
                history.clone(),
                [(successor.0.as_ref(), &successor.1)],
            )
            .map_err(|(height, error)| {
                anyhow!(
                    "header commitment verification failed at {}: {error}",
                    height.0
                )
            })?;
            let confirmed = (pending.0.hash(), pending.1.clone());
            let confirmed_height = pending.1.height.0;
            let mut next_history = history.clone();
            next_history.push_from_parts(&self.network, parts(&successor))?;
            // Recheck the selected successor after verification to catch a concurrent reorg.
            ensure!(
                Self::read(state, successor_height)?.0.hash() == successor.0.hash(),
                "selected header branch changed; retry sync"
            );
            self.verified.insert(confirmed_height, confirmed.clone());
            self.pending = Some((successor, next_history));
            if confirmed_height == height {
                return Ok(confirmed);
            }
        }
    }
    pub fn tree_state(&mut self, state: &ReadStateService, tree: &TreeState) -> Result<ChainState> {
        let height = u32::try_from(tree.height).context("tree height exceeds u32")?;
        let (hash, roots) = self.roots(state, height)?;
        let chain = tree.to_chain_state()?;
        check_frontiers(&chain, hash, &roots)?;
        Ok(chain)
    }
}
fn parts(data: &Data) -> HistoryTreeBlockParts<'_> {
    HistoryTreeBlockParts {
        header: &data.0,
        height: data.1.height,
        sapling_root: &data.1.sapling_root,
        orchard_root: &data.1.orchard_root,
        ironwood_root: &data.1.ironwood_root,
        sapling_tx: data.1.sapling_tx,
        orchard_tx: data.1.orchard_tx,
        ironwood_tx: data.1.ironwood_tx,
    }
}

fn check_frontiers(chain: &ChainState, hash: Hash, roots: &BlockCommitmentRoots) -> Result<()> {
    ensure!(
        chain.block_hash().0 == hash.0,
        "tree-state hash differs from the selected header"
    );
    ensure!(
        chain.final_sapling_tree().root().to_bytes() == <[u8; 32]>::from(roots.sapling_root),
        "Sapling frontier does not match authenticated root"
    );
    ensure!(
        chain.final_orchard_tree().root().to_bytes() == <[u8; 32]>::from(roots.orchard_root),
        "Orchard frontier does not match authenticated root"
    );
    ensure!(
        chain.final_ironwood_tree().root().to_bytes() == <[u8; 32]>::from(roots.ironwood_root),
        "Ironwood frontier does not match authenticated root"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_block_hash_does_not_authorize_a_different_ironwood_root() {
        let hash = Hash([7; 32]);
        let chain = ChainState::empty(2.into(), zcash_primitives::block::BlockHash(hash.0));
        let mut roots = BlockCommitmentRoots {
            height: Height(2),
            sapling_root: zakura_chain::sapling::tree::NoteCommitmentTree::default().root(),
            orchard_root: zakura_chain::orchard::tree::NoteCommitmentTree::default().root(),
            ironwood_root: zakura_chain::ironwood::tree::NoteCommitmentTree::default().root(),
            sapling_tx: 0,
            orchard_tx: 0,
            ironwood_tx: 0,
            auth_data_root: [0; 32].into(),
        };
        check_frontiers(&chain, hash, &roots).expect("the matching empty frontiers are valid");
        roots.ironwood_root = Default::default();
        let error = check_frontiers(&chain, hash, &roots).unwrap_err();
        assert!(error.to_string().contains("Ironwood frontier"));
    }
}
