use crate::{
    error::{Error, Result},
    public_ids::{Cursor, INITIAL_CURSOR},
};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::Path, sync::Mutex};

/// The only environment new records use. Rows of removed testing environments keep
/// their own value and are never read again (they are not deleted).
pub const ENV: &str = "production";
/// Schema version written to `PRAGMA user_version` by this release.
pub const SCHEMA_VERSION: i64 = 3;

/// All record bodies, including provider grants, jobs and results, are encrypted.
/// AAD binds ciphertext to its record and owner; indexes never contain secrets.
///
/// Accounts era: `org_id` and `owner_id` both hold the owning account's uuid (the
/// `org_id` column is the per-owner name namespace). Records written before
/// 0.3.0 keep their IAM-era values until `mcport-server link-identities` re-keys
/// them; that command keeps the originals in `legacy_org_id`/`legacy_owner_id` and
/// in each value's `legacy` object.
pub struct Store {
    pub(crate) db: Mutex<Connection>,
    cipher: Aes256Gcm,
}
impl Store {
    pub fn open(path: &Path, key: &[u8; 32]) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,environment TEXT NOT NULL,org_id TEXT NOT NULL,owner_id TEXT NOT NULL,name TEXT,revision INTEGER NOT NULL DEFAULT 1,value TEXT NOT NULL,PRIMARY KEY(kind,id),UNIQUE(kind,environment,org_id,name));
          CREATE INDEX IF NOT EXISTS tenant_records ON records(kind,environment,org_id,owner_id);")?;
        // Additive migration: preserve legacy public IDs and encrypted references.
        // The allocation ledger is deliberately outside environment cleanup.
        let migration = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        migration.execute_batch(
            "CREATE TABLE IF NOT EXISTS public_id_allocator(singleton INTEGER PRIMARY KEY CHECK(singleton=1),cursor TEXT NOT NULL,seed BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS public_ids(id TEXT PRIMARY KEY NOT NULL);
             CREATE TABLE IF NOT EXISTS public_id_replays(kind TEXT NOT NULL,replay_key TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(kind,replay_key),FOREIGN KEY(kind,id) REFERENCES records(kind,id) ON DELETE CASCADE);
             INSERT OR IGNORE INTO public_ids(id) SELECT id FROM records WHERE kind IN ('connection','host','call','report','directory','catalog');",
        )?;
        migration.execute(
            "INSERT OR IGNORE INTO public_id_allocator(singleton,cursor,seed) VALUES(1,?,?)",
            params![INITIAL_CURSOR, rand::random::<[u8; 32]>().as_slice()],
        )?;
        migration.commit()?;
        Self::migrate_schema(&mut db)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            db: Mutex::new(db),
            cipher: Aes256Gcm::new(key.into()),
        })
    }
    /// Additive, numbered schema steps gated by `PRAGMA user_version`. Each step runs
    /// in one IMMEDIATE transaction together with its version bump. Never edit an
    /// applied step and never drop or rewrite data here: identity re-keying is the
    /// explicit operator command `mcport-server link-identities`.
    fn migrate_schema(db: &mut Connection) -> Result<()> {
        const STEPS: [&str; SCHEMA_VERSION as usize] = [
            // 1: Silicon Accounts identity (0.3.0).
            "CREATE TABLE IF NOT EXISTS accounts(
                uuid TEXT PRIMARY KEY NOT NULL,
                kind TEXT NOT NULL DEFAULT '',
                id TEXT NOT NULL DEFAULT '',
                display_name TEXT NOT NULL DEFAULT '',
                pfp_url TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                custodian_uuid TEXT,
                custodian_id TEXT,
                version INTEGER NOT NULL DEFAULT 0,
                revoked_before INTEGER NOT NULL DEFAULT 0,
                synced_at_ms INTEGER NOT NULL DEFAULT 0,
                looked_up_at INTEGER NOT NULL DEFAULT 0,
                last_fid TEXT,
                updated_at INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS accounts_by_custodian ON accounts(custodian_uuid);
             CREATE TABLE IF NOT EXISTS identity_links(
                iam_principal_id TEXT PRIMARY KEY NOT NULL,
                iam_public_id TEXT NOT NULL,
                accounts_uuid TEXT NOT NULL,
                linked_at INTEGER NOT NULL,
                source TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS identity_links_by_uuid ON identity_links(accounts_uuid);
             CREATE TABLE IF NOT EXISTS identity_link_runs(
                run INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL,
                mapping_sha256 TEXT NOT NULL,
                dry_run INTEGER NOT NULL,
                report TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS accounts_webhook_events(
                event_id TEXT PRIMARY KEY NOT NULL,
                event_type TEXT NOT NULL,
                occurred_at_ms INTEGER,
                received_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS silicon_allowances(
                silicon_uuid TEXT NOT NULL,
                account_uuid TEXT NOT NULL,
                created_by TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(silicon_uuid, account_uuid));
             ALTER TABLE records ADD COLUMN legacy_id TEXT;
             ALTER TABLE records ADD COLUMN legacy_org_id TEXT;
             ALTER TABLE records ADD COLUMN legacy_owner_id TEXT;
             CREATE INDEX IF NOT EXISTS owner_records ON records(owner_id,kind);",
            "ALTER TABLE accounts ADD COLUMN signed_in_at INTEGER NOT NULL DEFAULT 0;",
            "CREATE TABLE account_uuid_migrations(old_uuid TEXT PRIMARY KEY NOT NULL,new_uuid TEXT UNIQUE NOT NULL,kind TEXT NOT NULL,migrated_at INTEGER NOT NULL);",
        ];
        for (index, step) in STEPS.iter().enumerate() {
            let version = index as i64 + 1;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if current >= version {
                continue;
            }
            tx.execute_batch(step)?;
            tx.execute_batch(&format!("PRAGMA user_version={version}"))?;
            tx.commit()?;
        }
        Ok(())
    }
    pub(crate) fn aad(kind: &str, id: &str, env: &str, org: &str, owner: &str) -> String {
        serde_json::json!([kind, id, env, org, owner]).to_string()
    }
    pub(crate) fn encrypt<T: Serialize>(&self, value: &T, aad: &str) -> Result<String> {
        let nonce: [u8; 12] = rand::random();
        let plain = serde_json::to_vec(value)?;
        let cipher = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Error::internal())?;
        let mut packed = nonce.to_vec();
        packed.extend(cipher);
        Ok(STANDARD.encode(packed))
    }
    pub(crate) fn decrypt<T: DeserializeOwned>(&self, value: &str, aad: &str) -> Result<T> {
        let packed = STANDARD.decode(value).map_err(|_| Error::internal())?;
        if packed.len() < 28 {
            return Err(Error::internal());
        }
        let plain = self
            .cipher
            .decrypt(
                Nonce::from_slice(&packed[..12]),
                Payload {
                    msg: &packed[12..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Error::internal())?;
        Ok(serde_json::from_slice(&plain)?)
    }
    /// Allocate and insert a public resource atomically. Rollback consumes no ID;
    /// committed IDs are never reused, including after deletion or environment cleanup.
    /// A replay key is private and is scoped by the caller before reaching this API.
    /// The closure must not call the store. The boolean reports a new record.
    #[allow(clippy::too_many_arguments)] // Explicit scope fields (environment, owner namespace, owner) bind encrypted values and replay lookup.
    pub fn create_public<T: Serialize + DeserializeOwned>(
        &self,
        kind: &str,
        env: &str,
        org: &str,
        owner: &str,
        name: Option<&str>,
        replay_key: Option<&str>,
        make: impl FnOnce(String) -> T,
    ) -> Result<(T, bool)> {
        if !matches!(
            kind,
            "connection" | "host" | "call" | "report" | "directory" | "catalog"
        ) {
            return Err(Error::internal());
        }
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = replay_key {
            // Previous versions stored keyed invocations directly under this hash.
            // Keep those records and uncertain-outcome replay behavior intact.
            let row: Option<(String, String)> = tx.query_row(
                "SELECT id,value FROM records WHERE kind=?1 AND environment=?2 AND org_id=?3 AND owner_id=?4 AND (id=(SELECT id FROM public_id_replays WHERE kind=?1 AND replay_key=?5) OR id=?5) LIMIT 1",
                params![kind, env, org, owner, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((id, value)) = row {
                let value = self.decrypt(&value, &Self::aad(kind, &id, env, org, owner))?;
                tx.commit()?;
                return Ok((value, false));
            }
        }
        let (cursor, seed): (String, Vec<u8>) = tx.query_row(
            "SELECT cursor,seed FROM public_id_allocator WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let seed: [u8; 32] = seed.try_into().map_err(|_| Error::internal())?;
        let mut cursor = Cursor::parse(&cursor)?;
        let id = loop {
            let candidate = cursor.public_id(&seed);
            cursor.next();
            // Only pre-existing legacy/imported identifiers can occupy a position
            // ahead of the cursor. Traverse the bijection, never random retries.
            if tx.execute(
                "INSERT OR IGNORE INTO public_ids(id) VALUES(?)",
                [&candidate],
            )? == 1
            {
                break candidate;
            }
        };
        let value = make(id.clone());
        let cipher = self.encrypt(&value, &Self::aad(kind, &id, env, org, owner))?;
        tx.execute(
            "INSERT INTO records(kind,id,environment,org_id,owner_id,name,value) VALUES(?,?,?,?,?,?,?)",
            params![kind, id, env, org, owner, name, cipher],
        )?;
        if let Some(key) = replay_key {
            tx.execute(
                "INSERT INTO public_id_replays(kind,replay_key,id) VALUES(?,?,?)",
                params![kind, key, id],
            )?;
        }
        tx.execute(
            "UPDATE public_id_allocator SET cursor=? WHERE singleton=1",
            [cursor.encoded()],
        )?;
        tx.commit()?;
        Ok((value, true))
    }
    #[allow(clippy::too_many_arguments)] // Explicit scope/AAD fields keep each write auditable.
    pub fn put<T: Serialize>(
        &self,
        kind: &str,
        id: &str,
        env: &str,
        org: &str,
        owner: &str,
        name: Option<&str>,
        value: &T,
        expected: Option<i64>,
    ) -> Result<()> {
        let cipher = self.encrypt(value, &Self::aad(kind, id, env, org, owner))?;
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction()?;
        let rev: Option<i64> = tx
            .query_row(
                "SELECT revision FROM records WHERE kind=? AND id=?",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(expected) = expected
            && rev.unwrap_or(0) != expected
        {
            return Err(Error::new(
                409,
                "revision_conflict",
                "This record changed while you were editing it.",
                "Refresh it and apply your change again.",
            ));
        }
        tx.execute("INSERT INTO records(kind,id,environment,org_id,owner_id,name,value) VALUES(?,?,?,?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET environment=excluded.environment,org_id=excluded.org_id,owner_id=excluded.owner_id,name=excluded.name,value=excluded.value,revision=records.revision+1",params![kind,id,env,org,owner,name,cipher])?;
        tx.commit()?;
        Ok(())
    }
    /// Persist a legacy private-to-invited update without activating dormant grants:
    /// the connection is stored only if its revision is still `expected`, and every
    /// grant on it is removed in the same transaction.
    pub fn put_connection_reset_grants<T: Serialize>(
        &self,
        id: &str,
        owner: &str,
        name: &str,
        connection: &T,
        expected: i64,
    ) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, String, String)> = tx
            .query_row(
                "SELECT environment,org_id,owner_id FROM records WHERE kind='connection' AND id=? AND revision=?",
                params![id, expected],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((env, org, stored_owner)) = row.filter(|(_, _, stored)| stored == owner) else {
            return Err(Error::new(
                409,
                "revision_conflict",
                "This connection changed.",
                "Refresh before applying the change.",
            ));
        };
        let cipher = self.encrypt(
            connection,
            &Self::aad("connection", id, &env, &org, &stored_owner),
        )?;
        tx.execute(
            "UPDATE records SET name=?,value=?,revision=revision+1 WHERE kind='connection' AND id=?",
            params![name, cipher, id],
        )?;
        self.delete_connection_records(&tx, id, &["grant"])?;
        tx.commit()?;
        Ok(())
    }
    /// Delete the records of `kinds` whose encrypted value names `connection_id`
    /// (grants, policies, credentials and OAuth attempts). A credential's provider
    /// account epoch goes with it.
    fn delete_connection_records(
        &self,
        tx: &rusqlite::Transaction<'_>,
        connection_id: &str,
        kinds: &[&str],
    ) -> Result<()> {
        for kind in kinds {
            let rows = {
                let mut statement = tx.prepare(
                    "SELECT id,environment,org_id,owner_id,value FROM records WHERE kind=?",
                )?;
                statement
                    .query_map([kind], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for (id, env, org, owner, cipher) in rows {
                let value: serde_json::Value =
                    self.decrypt(&cipher, &Self::aad(kind, &id, &env, &org, &owner))?;
                if value
                    .get("connection_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(connection_id)
                {
                    tx.execute(
                        "DELETE FROM records WHERE kind=? AND id=?",
                        params![kind, id],
                    )?;
                    if *kind == "credential" {
                        tx.execute(
                            "DELETE FROM records WHERE kind='account_epoch' AND id=?",
                            [&id],
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    /// Startup migration is atomic across all records and does not expand access.
    /// It reads values as JSON, so it works on every stored connection shape.
    pub fn migrate_private_visibility(&self) -> Result<usize> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = self.migrate_private_visibility_in(&tx)?;
        tx.commit()?;
        Ok(changed)
    }
    fn migrate_private_visibility_in(&self, tx: &rusqlite::Transaction<'_>) -> Result<usize> {
        let rows = {
            let mut statement = tx.prepare(
                "SELECT id,environment,org_id,owner_id,value FROM records WHERE kind='connection'",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut changed = 0;
        for (id, env, org, owner, cipher) in rows {
            let aad = Self::aad("connection", &id, &env, &org, &owner);
            let mut connection: serde_json::Value = self.decrypt(&cipher, &aad)?;
            if connection
                .get("visibility")
                .and_then(serde_json::Value::as_str)
                != Some("private")
            {
                continue;
            }
            self.delete_connection_records(tx, &id, &["grant"])?;
            connection["visibility"] = "invited".into();
            let version = connection
                .get("version")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            connection["version"] = (version + 1).into();
            connection["updated_at"] = crate::state::now().into();
            let cipher = self.encrypt(&connection, &aad)?;
            tx.execute(
                "UPDATE records SET value=?,revision=revision+1 WHERE kind='connection' AND id=?",
                params![cipher, id],
            )?;
            changed += 1;
        }
        Ok(changed)
    }
    /// Run `f` in one IMMEDIATE transaction with raw record access (operator
    /// commands). Commits only when `commit` is true; otherwise rolls back.
    pub fn raw_transaction<R>(
        &self,
        commit: bool,
        f: impl FnOnce(&RawTx<'_, '_>) -> Result<R>,
    ) -> Result<R> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = RawTx {
            tx: &tx,
            store: self,
        };
        let result = f(&raw)?;
        if commit {
            tx.commit()?;
        }
        Ok(result)
    }
    pub fn get<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>> {
        let row: Option<(String, String, String, String)> = self
            .db
            .lock()
            .map_err(|_| Error::internal())?
            .query_row(
                "SELECT environment,org_id,owner_id,value FROM records WHERE kind=? AND id=?",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        row.map(|(env, org, owner, value)| {
            self.decrypt(&value, &Self::aad(kind, id, &env, &org, &owner))
        })
        .transpose()
    }
    pub fn list<T: DeserializeOwned>(&self, kind: &str, env: Option<&str>) -> Result<Vec<T>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        let mut stmt=db.prepare("SELECT id,environment,org_id,owner_id,value FROM records WHERE kind=? AND (? IS NULL OR environment=?) ORDER BY rowid DESC")?;
        let rows = stmt
            .query_map(params![kind, env, env], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, env, org, owner, value)| {
                self.decrypt(&value, &Self::aad(kind, &id, &env, &org, &owner))
            })
            .collect()
    }
    /// Atomically change an existing encrypted record. The closure cannot access
    /// this store; use it to fence job completion, cancellation and progress.
    pub fn update<T: Serialize + DeserializeOwned>(
        &self,
        kind: &str,
        id: &str,
        change: impl FnOnce(&mut T) -> bool,
    ) -> Result<Option<T>> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction()?;
        let row: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT environment,org_id,owner_id,value FROM records WHERE kind=? AND id=?",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((env, org, owner, cipher)) = row else {
            return Ok(None);
        };
        let aad = Self::aad(kind, id, &env, &org, &owner);
        let mut value: T = self.decrypt(&cipher, &aad)?;
        if change(&mut value) {
            let encoded = self.encrypt(&value, &aad)?;
            tx.execute(
                "UPDATE records SET value=?,revision=revision+1 WHERE kind=? AND id=?",
                params![encoded, kind, id],
            )?;
        }
        tx.commit()?;
        Ok(Some(value))
    }
    pub fn delete(&self, kind: &str, id: &str) -> Result<()> {
        self.db.lock().map_err(|_| Error::internal())?.execute(
            "DELETE FROM records WHERE kind=? AND id=?",
            params![kind, id],
        )?;
        Ok(())
    }
    /// Remove a connection and all of its provider authority in one transaction.
    /// Retain call history; its read path still requires the live connection.
    pub fn delete_connection(&self, connection_id: &str) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.delete_connection_records(
            &tx,
            connection_id,
            &["credential", "grant", "policy", "oauth_attempt"],
        )?;
        tx.execute(
            "DELETE FROM records WHERE kind='connection' AND id=?",
            [connection_id],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// `(kind, id)` of every record of `kinds` whose owner column is `owner`.
    pub fn owned_by(&self, owner: &str, kinds: &[&str]) -> Result<Vec<(String, String)>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        let mut statement =
            db.prepare("SELECT kind,id FROM records WHERE owner_id=? ORDER BY rowid")?;
        let rows = statement
            .query_map([owner], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|(kind, _)| kinds.contains(&kind.as_str()))
            .collect())
    }
}

/// One stored record with its columns and decrypted value.
#[derive(Clone, Debug)]
pub struct RawRecord {
    pub rowid: i64,
    pub kind: String,
    pub id: String,
    pub environment: String,
    pub org_id: String,
    pub owner_id: String,
    pub name: Option<String>,
    pub legacy_id: Option<String>,
    pub legacy_org_id: Option<String>,
    pub legacy_owner_id: Option<String>,
    pub value: serde_json::Value,
}
/// Raw record access inside one transaction (see `Store::raw_transaction`).
pub struct RawTx<'t, 'c> {
    tx: &'t rusqlite::Transaction<'c>,
    store: &'t Store,
}
impl RawTx<'_, '_> {
    pub fn connection(&self) -> &rusqlite::Connection {
        self.tx
    }
    pub fn migrate_private_visibility(&self) -> Result<usize> {
        self.store.migrate_private_visibility_in(self.tx)
    }
    /// Every record of `kinds`, decrypted, in creation order.
    pub fn records(&self, kinds: &[&str]) -> Result<Vec<RawRecord>> {
        let mut statement = self.tx.prepare(
            "SELECT rowid,kind,id,environment,org_id,owner_id,name,legacy_id,legacy_org_id,legacy_owner_id,value FROM records ORDER BY rowid",
        )?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, String>(10)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (
            rowid,
            kind,
            id,
            environment,
            org_id,
            owner_id,
            name,
            legacy_id,
            legacy_org_id,
            legacy_owner_id,
            cipher,
        ) in rows
        {
            if !kinds.contains(&kind.as_str()) {
                continue;
            }
            let value = self.store.decrypt(
                &cipher,
                &Store::aad(&kind, &id, &environment, &org_id, &owner_id),
            )?;
            out.push(RawRecord {
                rowid,
                kind,
                id,
                environment,
                org_id,
                owner_id,
                name,
                legacy_id,
                legacy_org_id,
                legacy_owner_id,
                value,
            });
        }
        Ok(out)
    }
    /// Count records per kind (all environments).
    pub fn counts(&self) -> Result<Vec<(String, String, i64)>> {
        let mut statement = self.tx.prepare(
            "SELECT kind,environment,count(*) FROM records GROUP BY kind,environment ORDER BY kind,environment",
        )?;
        Ok(statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    /// Move a record out of the way: a temporary id and/or no name.
    pub fn park(&self, kind: &str, id: &str, temporary_id: Option<&str>) -> Result<()> {
        self.tx.execute(
            "UPDATE records SET id=coalesce(?,id),name=NULL WHERE kind=? AND id=?",
            params![temporary_id, kind, id],
        )?;
        Ok(())
    }
    /// Rewrite the record currently stored as (`kind`, `current_id`). Returns
    /// false (and changes nothing) when the new id or name is already taken.
    #[allow(clippy::too_many_arguments)] // Every column is explicit: they bind the AAD.
    pub fn rewrite(
        &self,
        kind: &str,
        current_id: &str,
        id: &str,
        org: &str,
        owner: &str,
        name: Option<&str>,
        value: &serde_json::Value,
        legacy: Option<(&str, &str, &str)>,
    ) -> Result<bool> {
        let cipher = self
            .store
            .encrypt(value, &Store::aad(kind, id, ENV, org, owner))?;
        let (legacy_id, legacy_org, legacy_owner) = match legacy {
            Some((id, org, owner)) => (Some(id), Some(org), Some(owner)),
            None => (None, None, None),
        };
        match self.tx.execute(
            "UPDATE records SET id=?,org_id=?,owner_id=?,name=?,value=?,legacy_id=?,legacy_org_id=?,legacy_owner_id=? WHERE kind=? AND id=?",
            params![id, org, owner, name, cipher, legacy_id, legacy_org, legacy_owner, kind, current_id],
        ) {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }
    /// Re-seal changed identity columns using the original environment and retain audit columns.
    pub fn remap_identity(
        &self,
        record: &RawRecord,
        id: &str,
        org: &str,
        owner: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let cipher = self.store.encrypt(
            value,
            &Store::aad(&record.kind, id, &record.environment, org, owner),
        )?;
        self.tx.execute(
            "UPDATE records SET id=?,org_id=?,owner_id=?,value=? WHERE rowid=?",
            params![id, org, owner, cipher, record.rowid],
        )?;
        Ok(())
    }
    /// Insert a new record unless (`kind`, `id`) exists. Returns whether it was inserted.
    pub fn insert(
        &self,
        kind: &str,
        id: &str,
        org: &str,
        owner: &str,
        value: &serde_json::Value,
    ) -> Result<bool> {
        let cipher = self
            .store
            .encrypt(value, &Store::aad(kind, id, ENV, org, owner))?;
        Ok(self.tx.execute(
            "INSERT OR IGNORE INTO records(kind,id,environment,org_id,owner_id,value) VALUES(?,?,?,?,?,?)",
            params![kind, id, ENV, org, owner, cipher],
        )? == 1)
    }
    pub fn delete(&self, kind: &str, id: &str) -> Result<()> {
        self.tx.execute(
            "DELETE FROM records WHERE kind=? AND id=?",
            params![kind, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{collections::HashSet, sync::Arc};

    fn resource(store: &Store, kind: &str, name: Option<&str>) -> (Value, bool) {
        store
            .create_public(
                kind,
                "test",
                "org",
                "owner",
                name,
                None,
                |id| json!({"id":id}),
            )
            .unwrap()
    }

    #[test]
    fn concurrent_resources_share_one_namespace_across_database_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let stores = [
            Arc::new(Store::open(&path, &[7; 32]).unwrap()),
            Arc::new(Store::open(&path, &[7; 32]).unwrap()),
        ];
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|worker| {
                let store = stores[worker % 2].clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..25)
                        .map(|index| {
                            let kind = ["connection", "host", "call", "report"][index % 4];
                            let (value, created) = resource(&store, kind, None);
                            assert!(created);
                            value["id"].as_str().unwrap().to_owned()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let ids: Vec<_> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(ids.len(), 200);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 200);
        assert!(ids.iter().all(|id| id.len() == 3));
        assert_eq!(
            stores[0]
                .db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM public_ids", [], |row| row
                    .get::<_, usize>(0),)
                .unwrap(),
            200
        );
    }

    #[test]
    fn deletion_cleanup_and_restart_never_reuse_committed_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let store = Store::open(&path, &[7; 32]).unwrap();
        let old: Vec<String> = ["connection", "host", "call", "report"]
            .map(|kind| {
                resource(&store, kind, None).0["id"]
                    .as_str()
                    .unwrap()
                    .into()
            })
            .into();
        store.delete_connection(&old[0]).unwrap();
        store.delete("host", &old[1]).unwrap();
        store.delete("call", &old[2]).unwrap();
        drop(store);
        let store = Store::open(&path, &[7; 32]).unwrap();
        let new: Vec<String> = ["connection", "host", "call", "report"]
            .map(|kind| {
                resource(&store, kind, None).0["id"]
                    .as_str()
                    .unwrap()
                    .into()
            })
            .into();
        assert!(new.iter().all(|id| !old.contains(id)));
        assert_eq!(old.into_iter().chain(new).collect::<HashSet<_>>().len(), 8);
    }

    #[test]
    fn database_allocation_crosses_exact_width_boundary_and_rolls_back_failures() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("test.sqlite"), &[7; 32]).unwrap();
        // Simulate 62^3-2 already-consumed positions; exhaustive bijection is
        // tested separately, without writing 238,328 fsynced fixture records.
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE public_id_allocator SET cursor='ZZY' WHERE singleton=1",
                [],
            )
            .unwrap();
        let first = resource(&store, "connection", Some("taken")).0;
        assert_eq!(first["id"].as_str().unwrap().len(), 3);
        let failed = store.create_public(
            "connection",
            "test",
            "org",
            "owner",
            Some("taken"),
            None,
            |id| json!({"id":id}),
        );
        assert!(failed.is_err());
        let second = resource(&store, "connection", Some("another")).0;
        assert_eq!(second["id"].as_str().unwrap().len(), 3);
        assert_ne!(first["id"], second["id"]);
        assert_eq!(
            resource(&store, "report", None).0["id"]
                .as_str()
                .unwrap()
                .len(),
            4
        );
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE public_id_allocator SET cursor='ZZZZ' WHERE singleton=1",
                [],
            )
            .unwrap();
        assert_eq!(
            resource(&store, "host", None).0["id"]
                .as_str()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            resource(&store, "call", None).0["id"]
                .as_str()
                .unwrap()
                .len(),
            5
        );
    }

    #[test]
    fn replay_mapping_is_atomic_private_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let stores = [
            Arc::new(Store::open(&path, &[7; 32]).unwrap()),
            Arc::new(Store::open(&path, &[7; 32]).unwrap()),
        ];
        let threads: Vec<_> = (0..8)
            .map(|worker| {
                let store = stores[worker % 2].clone();
                std::thread::spawn(move || {
                    store
                        .create_public(
                            "call",
                            "test",
                            "org",
                            "owner",
                            None,
                            Some("private-scoped-hash"),
                            |id| json!({"id":id,"fingerprint":"same"}),
                        )
                        .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
        assert!(results.iter().all(|(value, _)| value == &results[0].0));
        let id = results[0].0["id"].as_str().unwrap().to_owned();
        assert_eq!(id.len(), 3);
        drop(stores);
        let store = Store::open(&path, &[7; 32]).unwrap();
        let (replayed, created) = store
            .create_public::<Value>(
                "call",
                "test",
                "org",
                "owner",
                None,
                Some("private-scoped-hash"),
                |_| panic!("replay must not dispatch a new call"),
            )
            .unwrap();
        assert!(!created);
        assert_eq!(replayed["id"], id);
        store.delete("call", &id).unwrap();
        let (fresh, created) = store
            .create_public(
                "call",
                "test",
                "org",
                "owner",
                None,
                Some("private-scoped-hash"),
                |id| json!({"id":id}),
            )
            .unwrap();
        assert!(created);
        assert_ne!(fresh["id"], id);
    }

    #[test]
    fn additive_migration_keeps_legacy_ids_references_and_keyed_call_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let store = Store::open(&path, &[7; 32]).unwrap();
        let host = uuid::Uuid::new_v4().to_string();
        let connection = uuid::Uuid::new_v4().to_string();
        let legacy_call = "f".repeat(64);
        let call = json!({"id":legacy_call,"connection_id":connection,"host_id":host});
        for (kind, id, value) in [
            ("host", &host, json!({"id":host})),
            (
                "connection",
                &connection,
                json!({"id":connection,"host_id":host}),
            ),
            ("call", &legacy_call, call.clone()),
        ] {
            store
                .put(kind, id, "test", "org", "owner", None, &value, Some(0))
                .unwrap();
        }
        // Reproduce a pre-allocator database, retaining the encrypted records.
        store.db.lock().unwrap().execute_batch("DROP TABLE public_id_replays; DROP TABLE public_ids; DROP TABLE public_id_allocator;").unwrap();
        drop(store);
        let store = Store::open(&path, &[7; 32]).unwrap();
        assert_eq!(
            store
                .get::<Value>("connection", &connection)
                .unwrap()
                .unwrap()["host_id"],
            host
        );
        assert_eq!(
            store.get::<Value>("call", &legacy_call).unwrap().unwrap(),
            call
        );
        let (replayed, created) = store
            .create_public::<Value>(
                "call",
                "test",
                "org",
                "owner",
                None,
                Some(&legacy_call),
                |_| panic!("legacy call must remain replayable"),
            )
            .unwrap();
        assert!(!created);
        assert_eq!(replayed, call);
        assert_eq!(
            resource(&store, "connection", None).0["id"]
                .as_str()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn migration_reserves_existing_compact_ids_without_overwriting_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let store = Store::open(&path, &[7; 32]).unwrap();
        let seed: Vec<u8> = store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT seed FROM public_id_allocator", [], |row| row.get(0))
            .unwrap();
        let reserved = Cursor::parse(INITIAL_CURSOR)
            .unwrap()
            .public_id(&seed.try_into().unwrap());
        store
            .put(
                "host",
                &reserved,
                "test",
                "org",
                "owner",
                None,
                &json!({"id":reserved,"legacy":true}),
                Some(0),
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path, &[7; 32]).unwrap();
        let fresh = resource(&store, "connection", None).0;
        assert_ne!(fresh["id"], reserved);
        assert_eq!(
            store.get::<Value>("host", &reserved).unwrap().unwrap()["legacy"],
            true
        );
    }

    #[test]
    fn connection_deletion_removes_only_its_credentials_and_grants_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("test.sqlite"), &[7; 32]).unwrap();
        for connection in ["delete", "keep"] {
            store
                .put(
                    "connection",
                    connection,
                    "production",
                    "org",
                    "owner",
                    None,
                    &json!({"id":connection}),
                    None,
                )
                .unwrap();
            for kind in ["credential", "grant", "policy", "oauth_attempt"] {
                let id = format!("{connection}-{kind}");
                store
                    .put(
                        kind,
                        &id,
                        "production",
                        "org",
                        "owner",
                        None,
                        &json!({"connection_id":connection,"secret":"protected-fixture"}),
                        None,
                    )
                    .unwrap();
                if kind == "credential" {
                    store
                        .put(
                            "account_epoch",
                            &id,
                            "production",
                            "org",
                            "owner",
                            None,
                            &3,
                            None,
                        )
                        .unwrap();
                }
            }
        }
        store.delete_connection("delete").unwrap();
        assert!(
            store
                .get::<Value>("connection", "delete")
                .unwrap()
                .is_none()
        );
        assert!(store.get::<Value>("connection", "keep").unwrap().is_some());
        for kind in ["credential", "grant", "policy", "oauth_attempt"] {
            assert!(
                store
                    .get::<Value>(kind, &format!("delete-{kind}"))
                    .unwrap()
                    .is_none()
            );
            assert!(
                store
                    .get::<Value>(kind, &format!("keep-{kind}"))
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            store
                .get::<i64>("account_epoch", "delete-credential")
                .unwrap()
                .is_none()
        );
        let raw = std::fs::read(dir.path().join("test.sqlite")).unwrap();
        assert!(
            !raw.windows(b"protected-fixture".len())
                .any(|b| b == b"protected-fixture")
        );
    }
}

#[cfg(test)]
mod visibility_migration_tests {
    use super::*;
    use crate::connections::ConnectionRecord as McpConnection;
    use serde_json::{Value, json};
    /// A connection exactly as releases before 0.3.0 stored it.
    fn connection(id: &str, environment: &str, visibility: &str) -> Value {
        json!({"id":id,"name":id,"description":"","org_id":"tos","owner_id":"c:owner","environment":environment,"transport":"http","url":"https://example.com/mcp","host_id":null,"command":null,"args":[],"auth_mode":"shared","visibility":visibility,"status":"ready","can_manage":false,"account":null,"created_at":1,"updated_at":1,"version":1})
    }
    fn save(store: &Store, connection: &Value) {
        let id = connection["id"].as_str().unwrap();
        let environment = connection["environment"].as_str().unwrap();
        store
            .put(
                "connection",
                id,
                environment,
                "tos",
                "c:owner",
                Some(id),
                connection,
                Some(0),
            )
            .unwrap();
        store
            .put(
                "grant",
                &format!("grant-{id}"),
                environment,
                "tos",
                "c:owner",
                None,
                &json!({"connection_id":id,"grant":{"principal_id":"si:invitee","created_at":1}}),
                Some(0),
            )
            .unwrap();
        store
            .put(
                "credential",
                &format!("credential-{id}"),
                environment,
                "tos",
                "c:owner",
                None,
                &json!({"connection_id":id,"secret":"private-fixture"}),
                Some(0),
            )
            .unwrap();
    }
    #[test]
    fn migration_clears_only_dormant_grants_preserves_authority_and_is_restart_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let store = Store::open(&path, &[7; 32]).unwrap();
        for c in [
            connection("private-prod", "production", "private"),
            connection("private-test", "test-world", "private"),
            connection("invited", "production", "invited"),
            connection("org", "production", "org"),
        ] {
            save(&store, &c);
        }
        assert_eq!(store.migrate_private_visibility().unwrap(), 2);
        for id in ["private-prod", "private-test"] {
            let c = store
                .get::<McpConnection>("connection", id)
                .unwrap()
                .unwrap();
            assert_eq!(c.visibility, "invited");
            assert_eq!(c.version, 2);
            assert!(c.updated_at > 1);
            assert!(
                store
                    .get::<serde_json::Value>("grant", &format!("grant-{id}"))
                    .unwrap()
                    .is_none()
            );
            assert!(
                store
                    .get::<serde_json::Value>("credential", &format!("credential-{id}"))
                    .unwrap()
                    .is_some()
            );
        }
        for id in ["invited", "org"] {
            assert_eq!(
                store
                    .get::<McpConnection>("connection", id)
                    .unwrap()
                    .unwrap()
                    .version,
                1
            );
            assert!(
                store
                    .get::<serde_json::Value>("grant", &format!("grant-{id}"))
                    .unwrap()
                    .is_some()
            );
        }
        drop(store);
        let store = Store::open(&path, &[7; 32]).unwrap();
        assert_eq!(store.migrate_private_visibility().unwrap(), 0);
        assert_eq!(
            store
                .list::<McpConnection>("connection", None)
                .unwrap()
                .len(),
            4
        );
    }
    #[test]
    fn legacy_private_update_resets_grants_only_after_revision_check() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db.sqlite"), &[7; 32]).unwrap();
        let mut c = connection("existing", "production", "org");
        save(&store, &c);
        c["visibility"] = json!("invited");
        c["version"] = json!(2);
        assert_eq!(
            store
                .put_connection_reset_grants("existing", "c:owner", "existing", &c, 0)
                .unwrap_err()
                .1
                .code,
            "revision_conflict"
        );
        assert!(
            store
                .get::<serde_json::Value>("grant", "grant-existing")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .get::<McpConnection>("connection", "existing")
                .unwrap()
                .unwrap()
                .visibility,
            "org"
        );
        store
            .put_connection_reset_grants("existing", "c:owner", "existing", &c, 1)
            .unwrap();
        assert_eq!(
            store
                .get::<McpConnection>("connection", "existing")
                .unwrap()
                .unwrap()
                .visibility,
            "invited"
        );
        assert!(
            store
                .get::<serde_json::Value>("grant", "grant-existing")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get::<serde_json::Value>("credential", "credential-existing")
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn migration_rolls_back_all_changes_if_an_encrypted_grant_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db.sqlite"), &[7; 32]).unwrap();
        save(&store, &connection("first", "production", "private"));
        save(&store, &connection("second", "test-world", "private"));
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE records SET value='invalid' WHERE kind='grant' AND id='grant-second'",
                [],
            )
            .unwrap();
        assert!(store.migrate_private_visibility().is_err());
        for id in ["first", "second"] {
            let c = store
                .get::<McpConnection>("connection", id)
                .unwrap()
                .unwrap();
            assert_eq!(c.visibility, "private");
            assert_eq!(c.version, 1);
        }
        assert!(
            store
                .get::<serde_json::Value>("grant", "grant-first")
                .unwrap()
                .is_some()
        );
    }
}
