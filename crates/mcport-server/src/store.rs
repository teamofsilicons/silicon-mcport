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

/// All record bodies, including provider grants, jobs and results, are encrypted.
/// AAD binds ciphertext to its record and tenant; indexes never contain secrets.
pub struct Store {
    db: Mutex<Connection>,
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
             INSERT OR IGNORE INTO public_ids(id) SELECT id FROM records WHERE kind IN ('connection','host','call','report');",
        )?;
        migration.execute(
            "INSERT OR IGNORE INTO public_id_allocator(singleton,cursor,seed) VALUES(1,?,?)",
            params![INITIAL_CURSOR, rand::random::<[u8; 32]>().as_slice()],
        )?;
        migration.commit()?;
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
    fn aad(kind: &str, id: &str, env: &str, org: &str, owner: &str) -> String {
        serde_json::json!([kind, id, env, org, owner]).to_string()
    }
    fn encrypt<T: Serialize>(&self, value: &T, aad: &str) -> Result<String> {
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
    fn decrypt<T: DeserializeOwned>(&self, value: &str, aad: &str) -> Result<T> {
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
    #[allow(clippy::too_many_arguments)] // Explicit tenant fields bind encrypted values and replay lookup.
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
        if !matches!(kind, "connection" | "host" | "call" | "report") {
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
    #[allow(clippy::too_many_arguments)] // Explicit tenant/AAD fields keep each write auditable.
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
    pub fn delete_connection(&self, connection_id: &str, environment: &str) -> Result<()> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction()?;
        let rows = {
            let mut stmt = tx.prepare("SELECT kind,id,org_id,owner_id,value FROM records WHERE environment=? AND kind IN ('credential','grant','policy','oauth_attempt')")?;
            stmt.query_map([environment], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (kind, id, org, owner, cipher) in rows {
            let value: serde_json::Value =
                self.decrypt(&cipher, &Self::aad(&kind, &id, environment, &org, &owner))?;
            if value
                .get("connection_id")
                .and_then(serde_json::Value::as_str)
                == Some(connection_id)
            {
                tx.execute(
                    "DELETE FROM records WHERE kind=? AND id=?",
                    params![kind, id],
                )?;
                if kind == "credential" {
                    tx.execute(
                        "DELETE FROM records WHERE kind='account_epoch' AND id=?",
                        [&id],
                    )?;
                }
            }
        }
        tx.execute(
            "DELETE FROM records WHERE kind='connection' AND id=? AND environment=?",
            params![connection_id, environment],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn clear_environment(&self, env: &str) -> Result<()> {
        self.db.lock().map_err(|_| Error::internal())?.execute(
            "DELETE FROM records WHERE environment=? AND kind NOT IN ('environment','lifecycle')",
            [env],
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
        store.delete_connection(&old[0], "test").unwrap();
        store.delete("host", &old[1]).unwrap();
        store.clear_environment("test").unwrap();
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
        store.clear_environment("test").unwrap();
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
        store.delete_connection("delete", "production").unwrap();
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
