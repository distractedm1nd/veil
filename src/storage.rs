use std::{collections::HashSet, path::Path, time::Duration};

use anyhow::{Result, ensure};
use rand::{rand_core::UnwrapErr, rngs::SysRng};
use rusqlite::{Connection, OptionalExtension};
use schemerz_rusqlite::RusqliteMigration;
use uuid::Uuid;
use zcash_client_sqlite::{
    WalletDb,
    util::SystemClock,
    wallet::init::{WalletMigrationError, WalletMigrator},
};

use crate::{config::WalletNetwork, keys::private_file};

pub type Database = WalletDb<Connection, WalletNetwork, SystemClock, UnwrapErr<SysRng>>;

pub fn open(path: &Path, network: WalletNetwork) -> Result<Database> {
    if !path.exists() {
        private_file(path)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // Check our network identity before running migrations with network-specific data.
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'ext_veil_wallet')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        let saved: Option<String> = conn
            .query_row(
                "SELECT network FROM ext_veil_wallet WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(saved) = saved {
            ensure!(
                saved == serde_json::to_string(&network)?,
                "wallet belongs to a different network or activation schedule"
            );
        }
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    rusqlite::vtab::array::load_module(&conn)?;
    let mut db = WalletDb::from_connection(conn, network, SystemClock, UnwrapErr(SysRng));
    WalletMigrator::new()
        .with_external_migrations(vec![Box::new(InitialSchema)])
        .init_or_migrate(&mut db)?;
    Ok(db)
}

struct InitialSchema;

impl schemerz::Migration<Uuid> for InitialSchema {
    fn id(&self) -> Uuid {
        Uuid::from_u128(0xa8052f78_6583_45de_96b3_ef935c80e9b0)
    }
    fn dependencies(&self) -> HashSet<Uuid> {
        HashSet::new()
    }
    fn description(&self) -> &'static str {
        "Veil network identity and encrypted mnemonic"
    }
}

impl RusqliteMigration for InitialSchema {
    type Error = WalletMigrationError;
    fn up(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), Self::Error> {
        tx.execute_batch(
            "CREATE TABLE ext_veil_wallet (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            network TEXT NOT NULL,
            encrypted_mnemonic BLOB NOT NULL,
            birthday INTEGER NOT NULL,
            address TEXT NOT NULL
        );",
        )?;
        Ok(())
    }
    fn down(&self, _: &rusqlite::Transaction<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}
