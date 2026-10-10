//! Offline Accounts UUID backfill; never an authentication alias.
use crate::{
    accounts::AccountRow,
    connections::{credential_key, grant_key, policy_key},
    error::{Error, Result},
    identity_store,
    state::{App, Config, now},
    store::{RawRecord, Store},
};
use mcport_core::uuid_mapping::{self, Link, Mapping, mapped};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

impl Store {
    pub fn retired_account_uuid(&self, uuid: &str) -> Result<bool> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db
            .query_row(
                "SELECT 1 FROM account_uuid_migrations WHERE old_uuid=?",
                [uuid],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    /// Preserve opaque execution replay hashes without accepting old JWT subjects.
    pub fn replay_account_uuid(&self, uuid: &str) -> Result<String> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db
            .query_row(
                "SELECT old_uuid FROM account_uuid_migrations WHERE new_uuid=?",
                [uuid],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| uuid.into()))
    }
}
fn field(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::bad(format!("Stored migration field {key} is missing")))
}
fn remap_field(v: &mut Value, key: &str, m: &Mapping) {
    if let Some(old) = v.get(key).and_then(Value::as_str) {
        let new = mapped(m, old).to_owned();
        v[key] = json!(new);
    }
}
fn transformed(
    r: &RawRecord,
    m: &Mapping,
    epochs: &BTreeMap<String, String>,
) -> Result<(String, String, String, Value)> {
    let mut v = r.value.clone();
    let owner = mapped(m, &r.owner_id).to_owned();
    let org = mapped(m, &r.org_id).to_owned();
    // Only declared identity fields. Provider payloads, result artifacts and legacy audit snapshots are verbatim.
    let fields: &[&str] = match r.kind.as_str() {
        "connection" | "credential" | "report" | "directory" => &["owner_uuid"],
        "grant" | "directory_grant" => &["account_uuid", "created_by"],
        "policy" => &["account_uuid"],
        "call" => &["caller_uuid", "execution_account_uuid"],
        "oauth_attempt" => &["account_uuid"],
        _ => &[],
    };
    for key in fields {
        remap_field(&mut v, key, m);
    }
    if r.kind == "host" {
        remap_field(&mut v["host"], "owner_uuid", m);
        if let Some(capabilities) = v.get_mut("capabilities").and_then(Value::as_object_mut) {
            for capability in capabilities.values_mut().filter_map(Value::as_object_mut) {
                if let Some(owners) = capability
                    .get_mut("account_owners")
                    .and_then(Value::as_array_mut)
                {
                    for owner in owners {
                        if let Some(old) = owner.as_str() {
                            *owner = json!(mapped(m, old));
                        }
                    }
                }
                // Re-probe per-account provider health after the coordinated daemon restart.
                capability.remove("health");
            }
        }
    }
    if owner == r.owner_id && org == r.org_id && v == r.value && !epochs.contains_key(&r.id) {
        return Ok((r.id.clone(), org, owner, v));
    }
    let id = match r.kind.as_str() {
        "grant" => grant_key(&field(&v, "connection_id")?, &field(&v, "account_uuid")?),
        "directory_grant" => {
            crate::directory::entry_grant_key(&field(&v, "entry_id")?, &field(&v, "account_uuid")?)
        }
        "policy" => policy_key(
            &field(&v, "connection_id")?,
            &field(&v, "tool")?,
            v.get("account_uuid").and_then(Value::as_str),
        ),
        "credential" => credential_key(&field(&v, "connection_id")?, &field(&v, "owner_uuid")?),
        "settings" if owner != r.owner_id => crate::operations::settings_key(&owner),
        "account_epoch" => epochs.get(&r.id).cloned().unwrap_or_else(|| r.id.clone()),
        _ => r.id.clone(),
    };
    Ok((id, org, owner, v))
}
pub fn migrate(store: &Store, mapping: &Mapping, apply: bool) -> Result<Value> {
    store.raw_transaction(apply,|tx| {
        let db=tx.connection();
        let mut stmt=db.prepare("SELECT old_uuid,new_uuid,kind FROM account_uuid_migrations")?;
        let ledger:Mapping=stmt.query_map([],|r|Ok((r.get(0)?,Link {new_uuid:r.get(1)?,kind:r.get(2)?})))?.collect::<rusqlite::Result<_>>()?;
        let fresh=uuid_mapping::fresh(mapping,&ledger).map_err(Error::bad)?;
        let mut identities=db.prepare("SELECT uuid FROM accounts UNION SELECT custodian_uuid FROM accounts WHERE custodian_uuid IS NOT NULL UNION SELECT accounts_uuid FROM identity_links UNION SELECT silicon_uuid FROM silicon_allowances UNION SELECT account_uuid FROM silicon_allowances UNION SELECT created_by FROM silicon_allowances")?;
        for account in identities.query_map([], |r|r.get::<_,String>(0))? {
            uuid_mapping::require_covered(&account?,mapping,&ledger).map_err(Error::bad)?;
        }
        let kinds:Vec<_>=tx.counts()?.into_iter().map(|(kind,_,_)|kind).collect();
        let all=tx.records(&kinds.iter().map(String::as_str).collect::<Vec<_>>())?;
        for (old,link) in &fresh {
            if identity_store::read_account(db,old)?.is_some_and(|a| !a.kind.is_empty() && a.kind != link.kind) {return Err(Error::bad("Mapping kind conflicts with existing account"));}
            if identity_store::read_account(db,&link.new_uuid)?.is_some() || all.iter().any(|r|r.org_id==link.new_uuid || r.owner_id==link.new_uuid) {return Err(Error::bad("Target identity already exists; merging is forbidden"));}
        }
        if !fresh.is_empty() && all.iter().any(|r| r.kind=="call" && matches!(r.value["invocation"]["status"].as_str(),Some("queued"|"running"))) {return Err(Error::bad("Drain or cancel queued/running calls before UUID cutover; preserve daemon journals"));}
        for link in fresh.values() {
            let target=&link.new_uuid;
            let references:i64=db.query_row("SELECT (SELECT count(*) FROM accounts WHERE custodian_uuid=?1)+(SELECT count(*) FROM identity_links WHERE accounts_uuid=?1)+(SELECT count(*) FROM silicon_allowances WHERE silicon_uuid=?1 OR account_uuid=?1 OR created_by=?1)",[target],|r|r.get(0))?;
            if references>0 || all.iter().any(|r| ["owner_uuid","account_uuid","created_by","caller_uuid","execution_account_uuid"].iter().any(|k|r.value[*k].as_str()==Some(target.as_str())) || (r.kind=="host" && r.value["host"]["owner_uuid"].as_str()==Some(target.as_str()))) {
                return Err(Error::bad("Target identity already has linked data; merging is forbidden"));
            }
        }
        let mut epochs=BTreeMap::new();
        for c in all.iter().filter(|r|r.kind=="connection") { for (old,link) in &fresh {epochs.insert(credential_key(&c.id,old),credential_key(&c.id,&link.new_uuid));}}
        let mut changed=0; let mut expired=0;
        for r in &all {
            if r.kind=="oauth_attempt" && (fresh.contains_key(&r.owner_id) || fresh.contains_key(&r.org_id) || r.value["account_uuid"].as_str().is_some_and(|v|fresh.contains_key(v))) {tx.delete(&r.kind,&r.id)?; expired+=1;continue;}
            let (id,org,owner,value)=transformed(r,&fresh,&epochs)?;
            if id!=r.id || org!=r.org_id || owner!=r.owner_id || value!=r.value {
                tx.remap_identity(r,&id,&org,&owner,&value)?;changed+=1;
            }
        }
        for (old,link) in &fresh {
            db.execute("UPDATE accounts SET uuid=? WHERE uuid=?",params![link.new_uuid,old])?;
            db.execute("UPDATE accounts SET custodian_uuid=? WHERE custodian_uuid=?",params![link.new_uuid,old])?;
            db.execute("UPDATE identity_links SET accounts_uuid=? WHERE accounts_uuid=?",params![link.new_uuid,old])?;
            for column in ["silicon_uuid","account_uuid","created_by"] {db.execute(&format!("UPDATE silicon_allowances SET {column}=? WHERE {column}=?"),params![link.new_uuid,old])?;}
            db.execute("INSERT INTO account_uuid_migrations VALUES(?,?,?,?)",params![old,link.new_uuid,link.kind,now()])?;
            let mut fence=AccountRow::new(old,&link.kind);fence.status="deleted".into();fence.revoked_before=now();
            identity_store::write_account(db,&fence)?;
        }
        Ok(json!({"apply":apply,"mapping_rows":mapping.len(),"new_mappings":fresh.len(),"already_applied":mapping.len()-fresh.len(),"records_resealed":changed,"oauth_attempts_expired":expired,"reauthorization":"Sign in again; provider credentials and host tokens are preserved"}))
    })
}
pub fn run(config: Config, file: &Path, apply: bool) -> anyhow::Result<()> {
    for name in ["mcport.sqlite", "master.key"] {
        anyhow::ensure!(
            config.data_dir.join(name).is_file(),
            "MCPORT_DATA_DIR must name an existing store with {name}"
        );
    }
    let mapping =
        uuid_mapping::parse(&std::fs::read_to_string(file)?).map_err(anyhow::Error::msg)?;
    let app = App::new(config).map_err(|e| anyhow::anyhow!(e.1.message))?;
    let report = migrate(&app.store, &mapping, apply).map_err(|e| anyhow::anyhow!(e.1.message))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const CARBON: &str = "f858d0b5-98ba-4a4d-8ce5-114e93136f23";
    const SILICON: &str = "9ab26444-1f74-46bb-a734-235e98cb6f2d";
    fn mapping() -> Mapping {
        uuid_mapping::parse(&format!(
            "old_uuid,new_uuid,kind\nAda,{CARBON},carbon\nBot,{SILICON},silicon\n"
        ))
        .unwrap()
    }
    #[tokio::test]
    async fn dry_apply_replay_preserve_encrypted_data_and_reject_retired_subjects() {
        use crate::{store::ENV, test_support::fixture};
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.silicon("Bot", "si:bot", "Ada");
        assert_eq!(f.as_("Ada", "GET", "/api/v1/me", None).await.0, 200);
        assert_eq!(f.as_("Bot", "GET", "/api/v1/me", None).await.0, 200);
        let store = &f.app.store;
        let put = |kind: &str, id: &str, owner: &str, v: Value| {
            store
                .put(kind, id, ENV, "Ada", owner, None, &v, None)
                .unwrap()
        };
        put(
            "connection",
            "conn",
            "Ada",
            json!({"owner_uuid":"Ada","opaque":{"account_uuid":"Ada"}}),
        );
        put(
            "host",
            "host",
            "Ada",
            json!({"host":{"owner_uuid":"Ada"},"token_hash":"unchanged"}),
        );
        put(
            "credential",
            &credential_key("conn", "Bot"),
            "Bot",
            json!({"connection_id":"conn","owner_uuid":"Bot","access_token":"provider-secret","refresh_token":"provider-refresh"}),
        );
        put(
            "account_epoch",
            &credential_key("conn", "Bot"),
            "Bot",
            json!(7),
        );
        put(
            "grant",
            &grant_key("conn", "Bot"),
            "Ada",
            json!({"connection_id":"conn","account_uuid":"Bot","created_by":"Ada"}),
        );
        put(
            "policy",
            &policy_key("conn", "read", Some("Bot")),
            "Ada",
            json!({"connection_id":"conn","tool":"read","account_uuid":"Bot","enabled":false}),
        );
        put(
            "directory_grant",
            &crate::directory::entry_grant_key("entry", "Bot"),
            "Ada",
            json!({"entry_id":"entry","account_uuid":"Bot","created_by":"Ada"}),
        );
        put(
            "settings",
            &crate::operations::settings_key("Ada"),
            "Ada",
            json!({"telemetry":false}),
        );
        put(
            "oauth_attempt",
            "oauth",
            "Bot",
            json!({"account_uuid":"Bot","verifier":"expired-secret"}),
        );
        let replay = crate::state::hash(&json!(["Bot", "conn", "retry-key"]).to_string());
        let (call,_)=store.create_public("call",ENV,"Bot","Bot",None,Some(&replay),|id|json!({"invocation":{"id":id,"status":"completed","result":{"account_uuid":"Ada","bytes":"aGVsbG8="}},"caller_uuid":"Bot","execution_account_uuid":"Bot"})).unwrap();
        // An inert environment must be re-sealed with its own AAD, not production's.
        store
            .put(
                "report",
                "archived",
                "retired-test",
                "Ada",
                "Ada",
                None,
                &json!({"owner_uuid":"Ada","message":"Ada"}),
                None,
            )
            .unwrap();
        store.allow("Bot", "Ada", "Ada").unwrap();
        let map = mapping();
        let mut incomplete = map.clone();
        incomplete.remove("Bot");
        assert!(migrate(store, &incomplete, true).is_err());
        assert!(!store.retired_account_uuid("Ada").unwrap());
        let preview = migrate(store, &map, false).unwrap();
        assert_eq!(preview["new_mappings"], 2);
        assert!(!store.retired_account_uuid("Ada").unwrap());
        assert!(
            store
                .get::<Value>("credential", &credential_key("conn", "Bot"))
                .unwrap()
                .is_some()
        );
        let applied = migrate(store, &map, true).unwrap();
        assert_eq!(applied["oauth_attempts_expired"], 1);
        assert_eq!(
            store
                .account(SILICON)
                .unwrap()
                .unwrap()
                .custodian_uuid
                .as_deref(),
            Some(CARBON)
        );
        assert_eq!(store.allowances(SILICON).unwrap()[0].0, CARBON);
        let credential = store
            .get::<Value>("credential", &credential_key("conn", SILICON))
            .unwrap()
            .unwrap();
        assert_eq!(credential["access_token"], "provider-secret");
        assert_eq!(credential["refresh_token"], "provider-refresh");
        assert_eq!(
            store
                .get::<i64>("account_epoch", &credential_key("conn", SILICON))
                .unwrap(),
            Some(7)
        );
        assert!(
            store
                .get::<Value>("policy", &policy_key("conn", "read", Some(SILICON)))
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get::<Value>(
                    "directory_grant",
                    &crate::directory::entry_grant_key("entry", SILICON)
                )
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store.get::<Value>("connection", "conn").unwrap().unwrap()["opaque"]["account_uuid"],
            "Ada"
        );
        assert!(
            store
                .get::<Value>("oauth_attempt", "oauth")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.get::<Value>("report", "archived").unwrap().unwrap()["message"],
            "Ada"
        );
        let namespace = store.replay_account_uuid(SILICON).unwrap();
        assert_eq!(namespace, "Bot");
        let replay_new = crate::state::hash(&json!([namespace, "conn", "retry-key"]).to_string());
        let (restored, created) = store
            .create_public(
                "call",
                ENV,
                SILICON,
                SILICON,
                None,
                Some(&replay_new),
                |_| json!({"unexpected":true}),
            )
            .unwrap();
        assert!(!created);
        assert_eq!(restored["invocation"], call["invocation"]);
        assert_eq!(restored["caller_uuid"], SILICON);
        assert_eq!(migrate(store, &map, true).unwrap()["records_resealed"], 0);
        assert!(
            crate::identity::link(
                store,
                &crate::identity::Mapping::default(),
                &crate::identity::Known::new(),
                true
            )
            .is_err()
        );

        let conflict = uuid_mapping::parse(
            "old_uuid,new_uuid,kind\nAda,6fc8f876-9b80-419c-9835-1c5793588e8f,carbon\n",
        )
        .unwrap();
        assert!(migrate(store, &conflict, true).is_err());
        // Even a fresh, correctly signed old-subject JWT cannot resurrect old identity.
        assert_eq!(f.as_("Ada", "GET", "/api/v1/me", None).await.0, 401);
        f.carbon(CARBON, "c:ada");
        f.silicon(SILICON, "si:bot", CARBON);
        assert_eq!(f.as_(CARBON, "GET", "/api/v1/me", None).await.0, 200);
        assert_eq!(f.as_(SILICON, "GET", "/api/v1/me", None).await.0, 200);
        let old_fence = store.account("Bot").unwrap().unwrap();
        let migrated = store.account(SILICON).unwrap().unwrap();
        for subject in ["Bot", SILICON] {
            let (status, body) = f.webhook(&crate::test_support::event(
                &format!("retired-{subject}"),
                "silicon.custodian_changed",
                &crate::test_support::at(1),
                json!({"uuid":subject,"from":{"uuid":CARBON,"id":"c:ada"},"to":{"uuid":"Ada","id":"c:ada"}}),
            )).await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(body["ignored"], "account_uuid_migrated");
        }
        assert_eq!(store.account("Bot").unwrap().unwrap(), old_fence);
        assert_eq!(store.account(SILICON).unwrap().unwrap(), migrated);
    }
    #[test]
    fn target_collision_and_pending_work_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("test.sqlite"), &[19; 32]).unwrap();
        store
            .put(
                "grant",
                "existing",
                "production",
                "Else",
                "Else",
                None,
                &json!({"account_uuid":CARBON}),
                None,
            )
            .unwrap();
        assert!(migrate(&store, &mapping(), true).is_err());
        assert!(!store.retired_account_uuid("Ada").unwrap());
        store.delete("grant", "existing").unwrap();
        store
            .put(
                "call",
                "pending",
                "production",
                "Ada",
                "Ada",
                None,
                &json!({"invocation":{"status":"running"},"caller_uuid":"Ada"}),
                None,
            )
            .unwrap();
        assert!(migrate(&store, &mapping(), true).is_err());
        assert!(!store.retired_account_uuid("Ada").unwrap());
    }
}
