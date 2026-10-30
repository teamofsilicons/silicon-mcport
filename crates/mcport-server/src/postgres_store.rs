//! PostgreSQL schema and a verified, offline SQLite import. The private mcport
//! schema is never exposed through Cells' public PostgREST schema.
use crate::{database::Connection, error::Result, store::Store};
use rusqlite::{OptionalExtension, types::Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(crate) fn initialize(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS metadata(name TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS records(rowid BIGSERIAL UNIQUE NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,environment TEXT NOT NULL,org_id TEXT NOT NULL,owner_id TEXT NOT NULL,name TEXT,revision BIGINT NOT NULL DEFAULT 1,value TEXT NOT NULL,legacy_id TEXT,legacy_org_id TEXT,legacy_owner_id TEXT,PRIMARY KEY(kind,id),UNIQUE(kind,environment,org_id,name));
         CREATE INDEX IF NOT EXISTS tenant_records ON records(kind,environment,org_id,owner_id);
         CREATE INDEX IF NOT EXISTS owner_records ON records(owner_id,kind);
         CREATE TABLE IF NOT EXISTS public_id_allocator(singleton BIGINT PRIMARY KEY CHECK(singleton=1),cursor TEXT NOT NULL,seed BYTEA NOT NULL CHECK(octet_length(seed)=32));
         CREATE TABLE IF NOT EXISTS public_ids(id TEXT PRIMARY KEY NOT NULL);
         CREATE TABLE IF NOT EXISTS public_id_replays(kind TEXT NOT NULL,replay_key TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(kind,replay_key),FOREIGN KEY(kind,id) REFERENCES records(kind,id) ON DELETE CASCADE);
         CREATE TABLE IF NOT EXISTS accounts(uuid TEXT PRIMARY KEY,kind TEXT NOT NULL DEFAULT '',id TEXT NOT NULL DEFAULT '',display_name TEXT NOT NULL DEFAULT '',pfp_url TEXT,status TEXT NOT NULL DEFAULT 'active',custodian_uuid TEXT,custodian_id TEXT,version BIGINT NOT NULL DEFAULT 0,revoked_before BIGINT NOT NULL DEFAULT 0,synced_at_ms BIGINT NOT NULL DEFAULT 0,looked_up_at BIGINT NOT NULL DEFAULT 0,last_fid TEXT,updated_at BIGINT NOT NULL DEFAULT 0,signed_in_at BIGINT NOT NULL DEFAULT 0);
         CREATE INDEX IF NOT EXISTS accounts_by_custodian ON accounts(custodian_uuid);
         CREATE TABLE IF NOT EXISTS identity_links(iam_principal_id TEXT PRIMARY KEY,iam_public_id TEXT NOT NULL,accounts_uuid TEXT NOT NULL,linked_at BIGINT NOT NULL,source TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS identity_links_by_uuid ON identity_links(accounts_uuid);
         CREATE TABLE IF NOT EXISTS identity_link_runs(run BIGSERIAL PRIMARY KEY,started_at BIGINT NOT NULL,mapping_sha256 TEXT NOT NULL,dry_run BIGINT NOT NULL,report TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS accounts_webhook_events(event_id TEXT PRIMARY KEY,event_type TEXT NOT NULL,occurred_at_ms BIGINT,received_at BIGINT NOT NULL);
         CREATE TABLE IF NOT EXISTS silicon_allowances(silicon_uuid TEXT NOT NULL,account_uuid TEXT NOT NULL,created_by TEXT NOT NULL,created_at BIGINT NOT NULL,PRIMARY KEY(silicon_uuid,account_uuid));
         CREATE TABLE IF NOT EXISTS account_uuid_migrations(old_uuid TEXT PRIMARY KEY,new_uuid TEXT UNIQUE NOT NULL,kind TEXT NOT NULL,migrated_at BIGINT NOT NULL);
         REVOKE ALL ON ALL TABLES IN SCHEMA mcport FROM PUBLIC;
         REVOKE ALL ON ALL SEQUENCES IN SCHEMA mcport FROM PUBLIC;",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO metadata(name,value) VALUES('schema_version','1')",
        [],
    )?;
    let version: String = tx.query_row(
        "SELECT value FROM metadata WHERE name='schema_version'",
        [],
        |r| r.get(0),
    )?;
    if version != "1" {
        return Err(crate::error::Error::bad(
            "Unsupported PostgreSQL store schema",
        ));
    }
    tx.execute(
        "INSERT OR IGNORE INTO public_id_allocator(singleton,cursor,seed) VALUES(1,?,?)",
        rusqlite::params![
            crate::public_ids::INITIAL_CURSOR,
            rand::random::<[u8; 32]>().as_slice()
        ],
    )?;
    tx.commit()?;
    Ok(())
}

// Explicit columns make import independent of physical column order and ensure
// no implicit SQLite rowid or audit/identity/replay table is forgotten.
const TABLES: &[(&str, &str, &str)] = &[
    (
        "records",
        "rowid,kind,id,environment,org_id,owner_id,name,revision,value,legacy_id,legacy_org_id,legacy_owner_id",
        "rowid",
    ),
    ("public_ids", "id", "id"),
    ("public_id_allocator", "singleton,cursor,seed", "singleton"),
    ("public_id_replays", "kind,replay_key,id", "kind,replay_key"),
    (
        "accounts",
        "uuid,kind,id,display_name,pfp_url,status,custodian_uuid,custodian_id,version,revoked_before,synced_at_ms,looked_up_at,last_fid,updated_at,signed_in_at",
        "uuid",
    ),
    (
        "identity_links",
        "iam_principal_id,iam_public_id,accounts_uuid,linked_at,source",
        "iam_principal_id",
    ),
    (
        "identity_link_runs",
        "run,started_at,mapping_sha256,dry_run,report",
        "run",
    ),
    (
        "accounts_webhook_events",
        "event_id,event_type,occurred_at_ms,received_at",
        "event_id",
    ),
    (
        "silicon_allowances",
        "silicon_uuid,account_uuid,created_by,created_at",
        "silicon_uuid,account_uuid",
    ),
    (
        "account_uuid_migrations",
        "old_uuid,new_uuid,kind,migrated_at",
        "old_uuid",
    ),
];

fn digest(rows: &[Vec<Value>]) -> String {
    // Database collations may order mixed-case IDs differently. Hash canonical
    // typed row bytes, including explicit rowids, independently of collation.
    let mut encoded = Vec::with_capacity(rows.len());
    for row in rows {
        let mut out = Vec::new();
        out.extend_from_slice(&(row.len() as u64).to_be_bytes());
        for value in row {
            let (kind, bytes): (u8, Vec<u8>) = match value {
                Value::Null => (0, vec![]),
                Value::Integer(v) => (1, v.to_be_bytes().to_vec()),
                Value::Real(v) => (2, v.to_bits().to_be_bytes().to_vec()),
                Value::Text(v) => (3, v.as_bytes().to_vec()),
                Value::Blob(v) => (4, v.clone()),
            };
            out.push(kind);
            out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            out.extend_from_slice(&bytes);
        }
        encoded.push(out);
    }
    encoded.sort();
    let mut h = Sha256::new();
    for row in encoded {
        h.update((row.len() as u64).to_be_bytes());
        h.update(row);
    }
    hex::encode(h.finalize())
}

fn value_size(value: &Value) -> usize {
    match value {
        Value::Text(v) => v.len(),
        Value::Blob(v) => v.len(),
        _ => 8,
    }
}

pub fn import_sqlite(
    source: &Path,
    database_url: &str,
    key: &[u8; 32],
    apply: bool,
) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        source.is_file(),
        "Source must be an existing stopped SQLite snapshot"
    );
    let source_db =
        rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source_db.execute_batch("BEGIN")?;
    anyhow::ensure!(
        source_db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
        "Source integrity check failed"
    );
    anyhow::ensure!(
        source_db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?
            == crate::store::SCHEMA_VERSION,
        "Migrate the SQLite schema to the current version before importing"
    );
    anyhow::ensure!(
        source_db
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()?
            .is_none(),
        "Source foreign key check failed"
    );
    let mut tables = source_db.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let names = tables
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        names
            .iter()
            .all(|name| name == "sqlite_sequence"
                || TABLES.iter().any(|(table, _, _)| *table == name)),
        "Source contains an unrecognized durable table; extend and review the importer before proceeding"
    );
    // Validate the source key and drained execution state before touching the
    // destination at all. A mistyped key must not initialize a poisoned target.
    let mut ciphertexts =
        source_db.prepare("SELECT kind,id,environment,org_id,owner_id,value FROM records")?;
    for row in ciphertexts.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })? {
        let (kind, id, environment, org, owner, ciphertext) = row?;
        let value: serde_json::Value = Store::decrypt_with_key(
            key,
            &ciphertext,
            &Store::aad(&kind, &id, &environment, &org, &owner),
        )
        .map_err(|_| {
            anyhow::anyhow!("Source record authentication failed; verify the matching master key")
        })?;
        anyhow::ensure!(
            kind != "call"
                || !matches!(
                    value["invocation"]["status"].as_str(),
                    Some("queued" | "running")
                ),
            "Drain or cancel queued/running calls before importing; preserve the local daemon journals"
        );
    }
    let store =
        Store::open_postgres(database_url, key).map_err(|e| anyhow::anyhow!(e.1.message))?;
    let mut destination = store
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("Store lock unavailable"))?;
    let tx = destination.transaction()?;
    for (table, _, _) in TABLES
        .iter()
        .filter(|(t, _, _)| *t != "public_id_allocator")
    {
        let count: i64 =
            tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        anyhow::ensure!(
            count == 0,
            "Destination store is not empty; refusing to merge or overwrite"
        );
    }
    anyhow::ensure!(
        tx.query_row(
            "SELECT value FROM metadata WHERE name='sqlite_import'",
            [],
            |r| r.get::<_, String>(0)
        )
        .optional()?
        .is_none(),
        "This store was already imported; inspect it rather than repeating the import"
    );
    tx.execute("DELETE FROM public_id_allocator", [])?;
    let mut report = serde_json::Map::new();
    for (table, columns, order) in TABLES {
        let query = format!("SELECT {columns} FROM {table} ORDER BY {order}");
        let width = columns.split(',').count();
        let mut statement = source_db.prepare(&query)?;
        let rows = statement
            .query_map([], |r| {
                (0..width)
                    .map(|i| r.get(i))
                    .collect::<rusqlite::Result<Vec<Value>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if *table == "records" {
            for row in &rows {
                let s = |i| match &row[i] {
                    Value::Text(s) => Ok(s.as_str()),
                    _ => Err(anyhow::anyhow!("Invalid source record")),
                };
                let value: serde_json::Value = store
                    .decrypt(s(8)?, &Store::aad(s(1)?, s(2)?, s(3)?, s(4)?, s(5)?))
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "Source record authentication failed; verify the matching master key"
                        )
                    })?;
                anyhow::ensure!(
                    s(1)? != "call"
                        || !matches!(
                            value["invocation"]["status"].as_str(),
                            Some("queued" | "running")
                        ),
                    "Drain or cancel queued/running calls before importing; preserve the local daemon journals"
                );
            }
        }
        // Bound both parameters and copied payload bytes. Each batch is one
        // statement inside the same import transaction; WAN round trips do not
        // scale with every record or encrypted result.
        let tuple = format!("({})", vec!["?"; width].join(","));
        let mut start = 0;
        while start < rows.len() {
            let mut end = start;
            let mut bytes = 0;
            while end < rows.len() && end - start < 128 {
                let next = rows[end].iter().map(value_size).sum::<usize>();
                if end > start && bytes + next > 4 * 1024 * 1024 {
                    break;
                }
                bytes += next;
                end += 1;
            }
            let insert = format!(
                "INSERT INTO {table}({columns}) VALUES {}",
                vec![tuple.as_str(); end - start].join(",")
            );
            let values = rows[start..end]
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            tx.execute(&insert, values)?;
            start = end;
        }
        let mut check = tx.prepare(&query)?;
        let copied = check
            .query_map([], |r| Ok(r.values.clone()))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let expected = digest(&rows);
        anyhow::ensure!(
            rows.len() == copied.len() && expected == digest(&copied),
            "Imported table verification failed"
        );
        report.insert(
            (*table).into(),
            serde_json::json!({"rows": rows.len(), "sha256": expected}),
        );
    }
    if apply {
        tx.execute_batch("SELECT setval('records_rowid_seq', COALESCE((SELECT max(rowid) FROM records),1), EXISTS(SELECT 1 FROM records)); SELECT setval('identity_link_runs_run_seq', COALESCE((SELECT max(run) FROM identity_link_runs),1), EXISTS(SELECT 1 FROM identity_link_runs));")?;
    }
    tx.execute(
        "INSERT INTO metadata(name,value) VALUES('sqlite_import',?)",
        [serde_json::Value::Object(report.clone()).to_string()],
    )?;
    if apply {
        tx.commit()?;
    }
    Ok(
        serde_json::json!({"applied":apply,"verified":true,"tables":report,"encryption":"original ciphertext, AAD and master key preserved"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts::AccountRow, store::ENV};
    use serde_json::json;

    #[test]
    fn import_hash_is_independent_of_database_collation() {
        let a = vec![
            vec![Value::Text("a".into()), Value::Integer(1)],
            vec![Value::Text("B".into()), Value::Null],
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(digest(&a), digest(&b));
        b[0][1] = Value::Text("".into());
        assert_ne!(digest(&a), digest(&b));
    }

    #[test]
    #[ignore = "requires MCPORT_TEST_DATABASE_URL pointing to a disposable *_test database"]
    fn postgres_import_and_runtime_contract() {
        let url = std::env::var("MCPORT_TEST_DATABASE_URL").expect("set disposable PostgreSQL URL");
        let config: postgres::Config = url.parse().unwrap();
        assert!(
            config.get_dbname().is_some_and(|n| n.ends_with("_test")),
            "test refuses a database whose name does not end in _test"
        );
        let mut control = config.connect(postgres::NoTls).unwrap();
        control
            .batch_execute("DROP SCHEMA IF EXISTS mcport CASCADE")
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcport.sqlite");
        let key = [39; 32];
        let source = Store::open(&path, &key).unwrap();
        let (connection, _) = source
            .create_public(
                "connection",
                ENV,
                "alice",
                "alice",
                Some("docs"),
                None,
                |id| json!({"id":id,"owner_uuid":"alice","visibility":"invited","version":1}),
            )
            .unwrap();
        let id = connection["id"].as_str().unwrap();
        let (call, _) = source
            .create_public(
                "call",
                ENV,
                "alice",
                "alice",
                None,
                Some("opaque-idempotency-key"),
                |id| json!({"id":id,"invocation":{"status":"completed"},"result":"retained bytes"}),
            )
            .unwrap();
        source.put("credential","credential-id",ENV,"alice","alice",None,&json!({"connection_id":id,"access_token":"provider-secret","refresh_token":"provider-refresh"}),None).unwrap();
        source
            .put(
                "host",
                "host-id",
                ENV,
                "alice",
                "alice",
                None,
                &json!({"token_hash":"retained-host-credential"}),
                None,
            )
            .unwrap();
        source
            .update_account("alice", |_| Some(AccountRow::new("alice", "carbon")))
            .unwrap();
        source.allow("bot", "alice", "custodian").unwrap();
        source
            .record_webhook("delivered", "profile.updated", Some(1000))
            .unwrap();
        {
            let db = source.db.lock().unwrap();
            db.execute(
                "INSERT INTO identity_links VALUES('old-alice','c:alice','alice',1,'fixture')",
                [],
            )
            .unwrap();
            db.execute("INSERT INTO identity_link_runs(started_at,mapping_sha256,dry_run,report) VALUES(1,'digest',0,'{}')", []).unwrap();
            db.execute(
                "INSERT INTO account_uuid_migrations VALUES('old','new','carbon',1)",
                [],
            )
            .unwrap();
        }
        for i in 0..270 {
            source
                .put(
                    "policy",
                    &format!("policy-{i}"),
                    ENV,
                    "alice",
                    "alice",
                    None,
                    &json!({"connection_id":id,"enabled":true}),
                    None,
                )
                .unwrap();
        }
        drop(source);
        assert!(import_sqlite(&path, &url, &[40; 32], false).is_err());
        let exists: bool = control
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='mcport')",
                &[],
            )
            .unwrap()
            .get(0);
        assert!(
            !exists,
            "wrong source key must not initialize the destination"
        );
        let preview = import_sqlite(&path, &url, &key, false).unwrap();
        assert_eq!(preview["applied"], false);
        assert_eq!(preview["tables"]["records"]["rows"], 274);
        let report = import_sqlite(&path, &url, &key, true).unwrap();
        assert_eq!(preview["tables"], report["tables"]);
        assert!(
            import_sqlite(&path, &url, &key, true).is_err(),
            "reimport must not merge"
        );
        assert!(
            Store::open_postgres(&url, &[40; 32]).is_err(),
            "wrong key must refuse startup"
        );
        let store = Store::open_postgres(&url, &key).unwrap();
        assert!(
            Store::open_postgres(&url, &key).is_err(),
            "second writer must not start"
        );
        assert_eq!(
            store
                .get::<serde_json::Value>("connection", id)
                .unwrap()
                .unwrap(),
            connection
        );
        assert_eq!(
            store
                .get::<serde_json::Value>("credential", "credential-id")
                .unwrap()
                .unwrap()["access_token"],
            "provider-secret"
        );
        let (replay, created) = store
            .create_public::<serde_json::Value>(
                "call",
                ENV,
                "alice",
                "alice",
                None,
                Some("opaque-idempotency-key"),
                |_| panic!("must not execute twice"),
            )
            .unwrap();
        assert!(!created);
        assert_eq!(replay, call);
        assert!(
            store
                .put(
                    "connection",
                    id,
                    ENV,
                    "alice",
                    "alice",
                    Some("docs"),
                    &connection,
                    Some(99)
                )
                .is_err()
        );
        store
            .update::<serde_json::Value>("connection", id, |c| {
                c["version"] = json!(2);
                true
            })
            .unwrap();
        assert_eq!(
            store
                .get::<serde_json::Value>("connection", id)
                .unwrap()
                .unwrap()["version"],
            2
        );
        assert_eq!(
            store
                .list::<serde_json::Value>("connection", Some(ENV))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list::<serde_json::Value>("connection", None)
                .unwrap()
                .len(),
            1
        );
        assert!(store.allows("bot", "alice").unwrap());
        assert!(
            !store
                .record_webhook("delivered", "profile.updated", None)
                .unwrap()
        );
        assert!(
            store
                .record_webhook("new-event", "profile.updated", None)
                .unwrap()
        );
        store
            .update_account("alice", |a| {
                a.map(|mut a| {
                    a.display_name = "Alice".into();
                    a
                })
            })
            .unwrap();
        assert_eq!(
            store.account("alice").unwrap().unwrap().display_name,
            "Alice"
        );
        let next = store
            .create_public(
                "connection",
                ENV,
                "alice",
                "alice",
                Some("second"),
                None,
                |id| json!({"id":id}),
            )
            .unwrap()
            .0;
        assert_ne!(next["id"], connection["id"]);
        // Force a conflict within the same resource kind, then continue writing.
        store
            .raw_transaction(true, |tx| {
                assert!(!tx.rewrite(
                    "connection",
                    id,
                    next["id"].as_str().unwrap(),
                    "alice",
                    "alice",
                    Some("second"),
                    &connection,
                    None
                )?);
                tx.connection().execute(
                    "INSERT OR IGNORE INTO public_ids(id) VALUES(?)",
                    ["after-conflict"],
                )?;
                Ok(())
            })
            .unwrap();
        drop(store);
        let reopened = Store::open_postgres(&url, &key).unwrap();
        assert_eq!(
            reopened
                .get::<serde_json::Value>("call", call["id"].as_str().unwrap())
                .unwrap()
                .unwrap(),
            call
        );
        assert!(reopened.webhook_handled("new-event").unwrap());
        drop(reopened);
        control.batch_execute("DROP SCHEMA mcport CASCADE").unwrap();
    }
}
