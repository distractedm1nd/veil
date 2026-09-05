//! Serialized access to the library wallet database, outside Tokio worker threads.
use std::sync::{Arc, Mutex};

use crate::rpc::{Balance, TransactionSummary, WalletStatus};
use crate::{
    config::Config,
    keys::KeyStore,
    storage::{self, Database},
};
use anyhow::{Context, Result, anyhow, ensure};
use zcash_client_backend::{
    data_api::{
        AccountBirthday, WalletRead, WalletWrite, chain::ChainState, wallet::ConfirmationsPolicy,
    },
    proto::service::TreeState,
};
use zcash_keys::keys::{ReceiverRequirement, UnifiedAddressRequest};
use zcash_protocol::consensus::{NetworkUpgrade, Parameters};

pub struct Wallet {
    pub db: Database,
    pub keys: KeyStore,
    pub config: Config,
    _lock: std::fs::File,
}

impl Wallet {
    pub fn initialized(&self) -> Result<bool> {
        Ok(!self.db.get_account_ids()?.is_empty())
    }

    pub fn initialize(&mut self, mnemonic: &str, state: ChainState) -> Result<String> {
        ensure!(
            !self.initialized()?,
            "wallet already initialized; restore into a new data directory"
        );
        let birthday = AccountBirthday::from_parts(state, None);
        let activation = self
            .config
            .network
            .activation_height(NetworkUpgrade::Nu6_3)
            .context("Ironwood is not configured")?;
        ensure!(
            birthday.height() >= activation,
            "birthday must be at or after Ironwood activation"
        );
        let seed = crate::keys::seed(mnemonic)?;
        let ciphertext = self.keys.encrypt(mnemonic)?;
        let network = self.config.network;
        self.db.transactionally_with_extension(|db, ext| {
            let (_, usk) = db.create_account("veil", &seed, &birthday, Some("veil"))?;
            let request = UnifiedAddressRequest::custom(
                ReceiverRequirement::Require,
                ReceiverRequirement::Omit,
                ReceiverRequirement::Omit,
            )
            .expect("an Orchard-only receiver request is valid");
            let (address, _) = usk.to_unified_full_viewing_key().default_address(request)?;
            let address = address.encode(&network);
            ext.execute(
                "INSERT INTO ext_veil_wallet
                 (id, network, encrypted_mnemonic, birthday, address)
                 VALUES (1, ?1, ?2, ?3, ?4)",
                (
                    serde_json::to_string(&network)?,
                    ciphertext,
                    u32::from(birthday.height()),
                    &address,
                ),
            )?;
            Ok::<_, anyhow::Error>(address)
        })
    }

    pub fn initialize_from_tree(&mut self, mnemonic: &str, state: &TreeState) -> Result<String> {
        self.initialize(mnemonic, state.to_chain_state()?)
    }

    pub fn address(&mut self) -> Result<String> {
        self.db.transactionally_with_extension(|_, ext| {
            Ok(ext.query_row(
                "SELECT address FROM ext_veil_wallet WHERE id = 1",
                [],
                |row| row.get(0),
            )?)
        })
    }

    pub fn seed(&mut self) -> Result<secrecy::SecretVec<u8>> {
        let ciphertext: Vec<u8> = self.db.transactionally_with_extension(|_, ext| {
            Ok::<_, anyhow::Error>(ext.query_row(
                "SELECT encrypted_mnemonic FROM ext_veil_wallet WHERE id = 1",
                [],
                |row| row.get(0),
            )?)
        })?;
        crate::keys::seed(&self.keys.decrypt(&ciphertext)?)
    }

    pub fn status(&self) -> Result<WalletStatus> {
        Ok(WalletStatus {
            initialized: self.initialized()?,
            scanned_height: self
                .db
                .block_fully_scanned()?
                .map(|block| block.block_height().into()),
            sync_target: self.db.chain_height()?.map(Into::into),
            ..WalletStatus::default()
        })
    }

    pub fn balance(&self) -> Result<Balance> {
        let summary = self.db.get_wallet_summary(ConfirmationsPolicy::default())?;
        let Some(summary) = summary else {
            return Ok(Balance::default());
        };
        let mut result = Balance::default();
        for account in summary.account_balances().values() {
            result.total_zatoshis = result
                .total_zatoshis
                .checked_add(u64::from(account.ironwood_balance().total()))
                .context("balance overflow")?;
            result.spendable_zatoshis = result
                .spendable_zatoshis
                .checked_add(u64::from(account.ironwood_balance().spendable_value()))
                .context("balance overflow")?;
        }
        Ok(result)
    }

    pub fn transactions(&self) -> Result<Vec<TransactionSummary>> {
        // The library provides this view for wallet applications; no parallel history store.
        let conn = rusqlite::Connection::open_with_flags(
            self.config.wallet_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut query = conn.prepare(
            "SELECT DISTINCT txid, mined_height, COALESCE(expiry_height, 0)
             FROM v_transactions ORDER BY mined_height DESC, txid",
        )?;
        let rows = query.query_map([], |row| {
            let txid: Vec<u8> = row.get(0)?;
            let txid: [u8; 32] = txid.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(TransactionSummary {
                txid: zcash_primitives::transaction::TxId::from_bytes(txid).to_string(),
                mined_height: row.get(1)?,
                expiry_height: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[derive(Clone)]
pub struct WalletHandle(Arc<Mutex<Wallet>>);

impl WalletHandle {
    pub fn open(config: Config) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config.data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock_path = config.data_dir.join("daemon.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("another veild process owns this wallet")?;
        let new = !config.wallet_path().exists();
        let keys = KeyStore::open(&config.identity_path(), new)?;
        let db = storage::open(&config.wallet_path(), config.network)?;
        Ok(Self(Arc::new(Mutex::new(Wallet {
            db,
            keys,
            config,
            _lock: lock,
        }))))
    }

    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Wallet) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let wallet = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = wallet
                .lock()
                .map_err(|_| anyhow!("wallet worker panicked"))?;
            f(&mut guard)
        })
        .await?
    }
}
