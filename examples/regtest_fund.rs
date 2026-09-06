//! Regtest-only fixture: shield one mature coinbase directly into Ironwood.
//! The reference zcashd wallet still routes UAs to Orchard after NU6.3.
use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use std::{
    convert::Infallible,
    io::{Read, Write},
};
use transparent::{
    builder::{SpendInfo, TransparentInputInfo, TransparentSigningSet},
    bundle::OutPoint,
};
use veil::{config::WalletNetwork, send::NoSapling};
use zcash_keys::address::Address;
use zcash_primitives::transaction::{
    Transaction,
    builder::{BuildConfig, Builder, BundlePadding},
    fees::fixed,
};
use zcash_protocol::{consensus::BranchId, memo::MemoBytes, value::Zatoshis};

#[derive(Deserialize)]
struct Funding {
    address: String,
    coinbase: String,
    output_index: u32,
    secret_key: String,
    target_height: u32,
}
fn main() -> Result<()> {
    let mut input = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut input)?;
    let funding: Funding = serde_json::from_str(&input)?;
    let network = WalletNetwork::Regtest {
        ironwood_activation: 2,
    };
    let raw = hex::decode(funding.coinbase)?;
    let coinbase = Transaction::read(raw.as_slice(), BranchId::Nu6_2)?;
    let coin = coinbase
        .transparent_bundle()
        .context("coinbase has no transparent bundle")?
        .vout
        .get(usize::try_from(funding.output_index)?)
        .context("coinbase output is missing")?
        .clone();
    let key = zeroize::Zeroizing::new(
        bs58::decode(funding.secret_key)
            .with_check(Some(0xef))
            .into_vec()?,
    );
    ensure!(
        key.len() == 34 && key[33] == 1,
        "expected a compressed regtest WIF key"
    );
    let secret = secp256k1::SecretKey::from_slice(&key[1..33])?;
    let mut signing = TransparentSigningSet::new();
    let pubkey = signing.add_key(secret);
    let input = TransparentInputInfo::from_parts(
        OutPoint::new(*coinbase.txid().as_ref(), funding.output_index),
        coin.clone(),
        SpendInfo::P2pkh { pubkey },
    )
    .map_err(|e| anyhow!("funding input: {e}"))?;
    let Address::Unified(address) =
        Address::decode(&network, &funding.address).context("invalid regtest recipient")?
    else {
        anyhow::bail!("recipient must be unified")
    };
    let mut builder = Builder::new(
        network,
        funding.target_height.into(),
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(orchard::Anchor::empty_tree()),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    );
    builder.add_transparent_input(input);
    let fee = Zatoshis::from_u64(10_000)?;
    let amount = (coin.value() - fee).context("coinbase is smaller than the fee")?;
    builder
        .add_ironwood_output::<Infallible>(
            None,
            *address.orchard().context("missing Orchard receiver")?,
            amount,
            MemoBytes::empty(),
        )
        .map_err(|e| anyhow!("funding output: {e}"))?;
    let result = builder
        .build(
            &signing,
            &[],
            &[],
            rand::rand_core::UnwrapErr(rand::rngs::SysRng),
            &NoSapling,
            &NoSapling,
            &fixed::FeeRule::non_standard(fee),
        )
        .map_err(|e| anyhow!("funding transaction: {e}"))?;
    let mut raw = Vec::new();
    result.transaction().write(&mut raw)?;
    std::io::stdout().write_all(hex::encode(raw).as_bytes())?;
    Ok(())
}
