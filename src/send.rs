use crate::{
    rpc::{SendRequest, SendResult},
    wallet::Wallet,
};
use anyhow::{Context, Result, ensure};
use secrecy::ExposeSecret;
use std::{convert::Infallible, io::Write};
use zcash_client_backend::{
    data_api::{
        WalletRead,
        wallet::{
            ConfirmationsPolicy, SpendingKeys, create_proposed_transactions,
            input_selection::{GreedyInputSelector, SpendPolicy},
            propose_transfer,
        },
    },
    fees::{DustOutputPolicy, StandardFeeRule, zip317::SingleOutputChangeStrategy},
    wallet::OvkPolicy,
};
use zcash_keys::{address::Address, keys::UnifiedSpendingKey};
use zcash_protocol::{PoolType, ShieldedPool, value::Zatoshis};

impl Wallet {
    /// Builds a real signed transaction and captures it locally; no network submission.
    pub fn send(&mut self, request: SendRequest) -> Result<SendResult> {
        let params = self.config.network;
        let recipient = Address::decode(&params, &request.address)
            .context("invalid address for this network")?;
        ensure!(
            matches!(&recipient, Address::Unified(ua) if ua.orchard().is_some()),
            "recipient must have an Orchard receiver for Ironwood"
        );
        let amount = Zatoshis::from_u64(request.amount_zatoshis)?;
        ensure!(amount > Zatoshis::ZERO, "amount must be positive");
        let account = self
            .db
            .get_account_ids()?
            .into_iter()
            .next()
            .context("wallet is not initialized")?;
        let payment = zip321::Payment::new(
            recipient.to_zcash_address(&params),
            Some(amount),
            None,
            None,
            None,
            vec![],
        )?;
        let request = zip321::TransactionRequest::new(vec![payment])?;
        let change = SingleOutputChangeStrategy::new(
            StandardFeeRule::Zip317,
            None,
            ShieldedPool::Ironwood,
            DustOutputPolicy::default(),
        );
        let proposal = propose_transfer::<_, _, _, _, Infallible>(
            &mut self.db,
            &params,
            account,
            &GreedyInputSelector::new(),
            &change,
            request,
            ConfirmationsPolicy::default(),
            &SpendPolicy::shielded_pools([ShieldedPool::Ironwood]),
            None,
            None,
        )
        .map_err(|e| anyhow::anyhow!("proposal: {e}"))?;
        ensure!(
            proposal.steps().len() == 1,
            "veil supports single-step transfers"
        );
        ensure!(
            proposal.steps().iter().all(|s| s
                .payment_pools()
                .values()
                .all(|pool| *pool == PoolType::Shielded(ShieldedPool::Ironwood))),
            "proposal contains a non-Ironwood payment"
        );
        let seed = self.seed()?;
        let usk =
            UnifiedSpendingKey::from_seed(&params, seed.expose_secret(), zip32::AccountId::ZERO)?;
        let txids = create_proposed_transactions::<_, _, Infallible, _, Infallible, _>(
            &mut self.db,
            &params,
            &NoSapling,
            &NoSapling,
            &SpendingKeys::from_unified_spending_key(usk),
            OvkPolicy::Sender,
            &proposal,
            None,
        )
        .map_err(|e| anyhow::anyhow!("build transaction: {e}"))?;
        let txid = txids.head;
        let tx = self
            .db
            .get_transaction(txid)?
            .context("built transaction was not stored")?;
        let directory = self.config.data_dir.join("mock-submissions");
        std::fs::create_dir_all(&directory)?;
        let mut raw = Vec::new();
        tx.write(&mut raw)?;
        let mut file = crate::keys::private_file(&directory.join(format!("{txid}.tx")))?;
        file.write_all(&raw)?;
        file.sync_all()?;
        Ok(SendResult {
            txid: txid.to_string(),
            submission: "mocked".into(),
        })
    }
}

// The library's builder requires Sapling prover parameters even for an Ironwood-only
// proposal. An uninhabited proof type prevents accidentally producing dummy proofs.
#[doc(hidden)]
pub struct NoSapling;
impl sapling::prover::SpendProver for NoSapling {
    type Proof = Infallible;
    fn prepare_circuit(
        _: sapling::ProofGenerationKey,
        _: sapling::Diversifier,
        _: sapling::Rseed,
        _: sapling::value::NoteValue,
        _: jubjub::Fr,
        _: sapling::value::ValueCommitTrapdoor,
        _: bls12_381::Scalar,
        _: sapling::MerklePath,
    ) -> Option<sapling::circuit::Spend> {
        None
    }
    fn create_proof<R: rand::rand_core::Rng>(
        &self,
        _: sapling::circuit::Spend,
        _: &mut R,
    ) -> Infallible {
        unreachable!("Ironwood-only proposals contain no Sapling spends")
    }
    fn encode_proof(proof: Infallible) -> sapling::bundle::GrothProofBytes {
        match proof {}
    }
}
impl sapling::prover::OutputProver for NoSapling {
    type Proof = Infallible;
    fn prepare_circuit(
        _: &sapling::keys::EphemeralSecretKey,
        _: sapling::PaymentAddress,
        _: jubjub::Fr,
        _: sapling::value::NoteValue,
        _: sapling::value::ValueCommitTrapdoor,
    ) -> sapling::circuit::Output {
        unreachable!("Ironwood-only proposals contain no Sapling outputs")
    }
    fn create_proof<R: rand::rand_core::Rng>(
        &self,
        _: sapling::circuit::Output,
        _: &mut R,
    ) -> Infallible {
        unreachable!("Ironwood-only proposals contain no Sapling outputs")
    }
    fn encode_proof(proof: Infallible) -> sapling::bundle::GrothProofBytes {
        match proof {}
    }
}
