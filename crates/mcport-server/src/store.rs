use crate::error::{Error, Result};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
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
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,environment TEXT NOT NULL,org_id TEXT NOT NULL,owner_id TEXT NOT NULL,name TEXT,revision INTEGER NOT NULL DEFAULT 1,value TEXT NOT NULL,PRIMARY KEY(kind,id),UNIQUE(kind,environment,org_id,name));
          CREATE INDEX IF NOT EXISTS tenant_records ON records(kind,environment,org_id,owner_id);")?;
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
