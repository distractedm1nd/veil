use secrecy::ExposeSecret;
use veil::{
    config::{Config, WalletNetwork},
    keys,
    wallet::WalletHandle,
};
use zcash_client_backend::data_api::chain::ChainState;
use zcash_primitives::block::BlockHash;

#[tokio::test]
async fn encrypted_wallet_survives_restart_and_restores_the_same_address() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let config = Config {
        data_dir: directory.path().join("original"),
        network: WalletNetwork::Regtest {
            ironwood_activation: 2,
        },
        ..Default::default()
    };
    let phrase = keys::generate_mnemonic();
    let seed = keys::seed(&phrase)?;
    let wallet = WalletHandle::open(config.clone())?;
    assert!(
        WalletHandle::open(config.clone()).is_err(),
        "a second daemon must not open the wallet"
    );
    let phrase_copy = phrase.clone();
    let address = wallet
        .call(move |w| {
            w.initialize(
                &phrase_copy,
                ChainState::empty(1.into(), BlockHash([0; 32])),
            )
        })
        .await?;
    assert_eq!(
        wallet.call(|w| w.seed()).await?.expose_secret(),
        seed.expose_secret()
    );
    let connection = rusqlite::Connection::open(config.wallet_path())?;
    let ciphertext: Vec<u8> = connection.query_row(
        "SELECT encrypted_mnemonic FROM ext_veil_wallet",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !ciphertext
            .windows(phrase.len())
            .any(|part| part == phrase.as_bytes())
    );
    drop(connection);
    drop(wallet);
    let reopened = WalletHandle::open(config.clone())?;
    assert_eq!(reopened.call(|w| w.address()).await?, address);
    assert!(reopened.call(|w| w.initialized()).await?);
    assert_eq!(
        reopened.call(|w| w.seed()).await?.expose_secret(),
        seed.expose_secret()
    );
    drop(reopened);
    let mut wrong_network = config.clone();
    wrong_network.network = WalletNetwork::Regtest {
        ironwood_activation: 3,
    };
    assert!(WalletHandle::open(wrong_network).is_err());
    let restored = WalletHandle::open(Config {
        data_dir: directory.path().join("restored"),
        ..config
    })?;
    let restored_address = restored
        .call(move |w| w.initialize(&phrase, ChainState::empty(1.into(), BlockHash([0; 32]))))
        .await?;
    assert_eq!(address, restored_address);
    Ok(())
}
