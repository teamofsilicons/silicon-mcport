//! Cutover from IAM-era principal ids (`c:saket` in org `tos`) to Silicon
//! Accounts uuids. Operator commands only; the service never runs this.
//!
//! `link-identities` is a pure function of the original records and the mapping
//! file: every rewritten record keeps its original identity (columns
//! `legacy_id`/`legacy_org_id`/`legacy_owner_id` and the value's `legacy` object),
//! so re-running with the same mapping changes nothing and re-running with a
//! different one recomputes everything from the originals (a principal left out
//! of the mapping gets its original record back). Nothing is deleted except
//! grants this command itself created in an earlier run.
use crate::{
    accounts::AccountRow,
    connections::{credential_key, grant_key, policy_key},
    error::{Error, Result},
    identity_store,
    state::{App, Config, hash, now},
    store::{ENV, RawRecord, RawTx, Store},
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
};

/// Record kinds that carry identity and are re-keyed.
const KINDS: [&str; 9] = [
    "connection",
    "host",
    "grant",
    "policy",
    "credential",
    "call",
    "settings",
    "report",
    "directory",
];
/// Kinds whose record id is derived from identity (it changes when re-keyed).
const DERIVED: [&str; 4] = ["grant", "policy", "credential", "settings"];
/// Kinds whose `name` column is unique per owner.
const NAMED: [&str; 3] = ["connection", "host", "directory"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub principal: String,
    pub public_id: String,
    pub uuid: String,
}
#[derive(Clone, Debug, Default)]
pub struct Mapping {
    pub links: BTreeMap<String, Link>,
    pub sha256: String,
}
impl Mapping {
    pub fn uuid(&self, principal: &str) -> Option<&str> {
        self.links.get(principal).map(|link| link.uuid.as_str())
    }
}

/// Parse `iam_principal_id,accounts_uuid[,iam_public_id]` CSV (header required).
pub fn parse_mapping(text: &str) -> std::result::Result<Mapping, String> {
    let mut links = BTreeMap::new();
    let mut header = None;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if header.is_none() {
            if fields != ["iam_principal_id", "accounts_uuid"]
                && fields != ["iam_principal_id", "accounts_uuid", "iam_public_id"]
            {
                return Err(format!(
                    "line {number}: the mapping file must start with the header `iam_principal_id,accounts_uuid` (optionally `,iam_public_id`); got `{line}`."
                ));
            }
            header = Some(fields.len());
            continue;
        }
        if Some(fields.len()) != header {
            return Err(format!(
                "line {number}: expected {} comma-separated values, got {}.",
                header.unwrap_or(2),
                fields.len()
            ));
        }
        let (principal, uuid) = (fields[0], fields[1]);
        if principal.is_empty() || principal.len() > 200 || principal.chars().any(char::is_control)
        {
            return Err(format!(
                "line {number}: iam_principal_id is empty or invalid."
            ));
        }
        if !crate::accounts::valid_account_uuid(uuid) {
            return Err(format!(
                "line {number}: accounts_uuid `{uuid}` is not a Silicon Accounts uuid (canonical 128-bit UUID or a case-sensitive legacy key)."
            ));
        }
        let public_id = fields
            .get(2)
            .filter(|id| !id.is_empty())
            .unwrap_or(&principal);
        if links
            .insert(
                principal.to_owned(),
                Link {
                    principal: principal.into(),
                    public_id: (*public_id).into(),
                    uuid: uuid.into(),
                },
            )
            .is_some()
        {
            return Err(format!(
                "line {number}: iam_principal_id `{principal}` appears twice."
            ));
        }
    }
    if header.is_none() {
        return Err("The mapping file is empty: it needs the header `iam_principal_id,accounts_uuid` and one line per principal.".into());
    }
    Ok(Mapping {
        links,
        sha256: hash(text),
    })
}

// ---- reversible value edits ----------------------------------------------------

fn get_path<'v>(value: &'v Value, path: &str) -> Option<&'v Value> {
    path.split('.').try_fold(value, |v, key| v.get(key))
}
fn parent_mut<'v>(
    value: &'v mut Value,
    path: &str,
) -> Option<(&'v mut Map<String, Value>, String)> {
    let mut parts: Vec<&str> = path.split('.').collect();
    let last = parts.pop()?.to_owned();
    let mut current = value;
    for part in parts {
        current = current.get_mut(part)?;
    }
    Some((current.as_object_mut()?, last))
}
/// Edits that remember what they changed, so `inverse` restores the original.
struct Edit {
    value: Value,
    fields: Map<String, Value>,
    added: Vec<String>,
}
impl Edit {
    fn new(value: Value) -> Self {
        Self {
            value,
            fields: Map::new(),
            added: vec![],
        }
    }
    fn remember(&mut self, path: &str) {
        if self.fields.contains_key(path) || self.added.iter().any(|p| p == path) {
            return;
        }
        match get_path(&self.value, path) {
            Some(original) => {
                self.fields.insert(path.into(), original.clone());
            }
            None => self.added.push(path.into()),
        }
    }
    fn set(&mut self, path: &str, value: Value) {
        self.remember(path);
        if let Some((parent, key)) = parent_mut(&mut self.value, path) {
            parent.insert(key, value);
        }
    }
    fn take(&mut self, path: &str) {
        if get_path(&self.value, path).is_none() {
            return;
        }
        self.remember(path);
        if let Some((parent, key)) = parent_mut(&mut self.value, path) {
            parent.remove(&key);
        }
    }
    fn finish(mut self, original_id: &str, original_org: &str, original_owner: &str) -> Value {
        if let Some(object) = self.value.as_object_mut() {
            object.insert(
                "legacy".into(),
                json!({"fields": self.fields, "added": self.added, "id": original_id, "org_id": original_org, "owner_id": original_owner}),
            );
        }
        self.value
    }
}
/// The original value of a record a previous run rewrote.
fn inverse(value: &Value) -> Value {
    let mut value = value.clone();
    let Some(legacy) = value.as_object_mut().and_then(|o| o.remove("legacy")) else {
        return value;
    };
    for path in legacy
        .get("added")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some((parent, key)) = parent_mut(&mut value, path) {
            parent.remove(&key);
        }
    }
    if let Some(fields) = legacy.get("fields").and_then(Value::as_object) {
        for (path, original) in fields {
            if let Some((parent, key)) = parent_mut(&mut value, path) {
                parent.insert(key, original.clone());
            }
        }
    }
    value
}
fn text(value: &Value, path: &str) -> Option<String> {
    get_path(value, path)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A record as it was before any re-key.
#[derive(Clone)]
struct Original {
    id: String,
    org: String,
    owner: String,
    name: Option<String>,
    value: Value,
}
fn original(record: &RawRecord) -> Original {
    if record.legacy_owner_id.is_some() {
        let value = inverse(&record.value);
        let name = if NAMED.contains(&record.kind.as_str()) {
            text(
                &value,
                if record.kind == "host" {
                    "host.name"
                } else {
                    "name"
                },
            )
        } else {
            None
        };
        Original {
            id: record
                .legacy_id
                .clone()
                .unwrap_or_else(|| record.id.clone()),
            org: record.legacy_org_id.clone().unwrap_or_default(),
            owner: record.legacy_owner_id.clone().unwrap_or_default(),
            name,
            value,
        }
    } else {
        Original {
            id: record.id.clone(),
            org: record.org_id.clone(),
            owner: record.owner_id.clone(),
            name: record.name.clone(),
            value: record.value.clone(),
        }
    }
}
/// What a record should become.
#[derive(Clone, PartialEq)]
struct Target {
    id: String,
    org: String,
    owner: String,
    name: Option<String>,
    value: Value,
    linked: bool,
}
impl Target {
    fn restore(original: &Original) -> Self {
        Self {
            id: original.id.clone(),
            org: original.org.clone(),
            owner: original.owner.clone(),
            name: original.name.clone(),
            value: original.value.clone(),
            linked: false,
        }
    }
}

/// What the re-key knows about each mapped account (from Accounts, or offline
/// from the id prefix with no custodian).
pub type Known = BTreeMap<String, AccountRow>;

struct Plan<'a> {
    mapping: &'a Mapping,
    known: &'a Known,
    /// Linked connections: id → (owner uuid, original owner principal, original visibility, auth mode).
    connections: BTreeMap<String, (String, String, String, String)>,
}
impl Plan<'_> {
    fn forward(&self, kind: &str, o: &Original) -> Option<Target> {
        let v = &o.value;
        let mut e = Edit::new(v.clone());
        let (id, org, owner) = match kind {
            "connection" => {
                let uuid = self
                    .mapping
                    .uuid(&text(v, "owner_id").unwrap_or_else(|| o.owner.clone()))?;
                e.take("owner_id");
                e.take("org_id");
                e.take("environment");
                e.set("owner_uuid", json!(uuid));
                match text(v, "visibility").as_deref() {
                    Some("org") => e.set("visibility", json!("circle")),
                    Some("private") => e.set("visibility", json!("invited")),
                    _ => {}
                }
                (o.id.clone(), uuid.to_owned(), uuid.to_owned())
            }
            "host" => {
                let uuid = self
                    .mapping
                    .uuid(&text(v, "host.owner_id").unwrap_or_else(|| o.owner.clone()))?;
                e.take("host.owner_id");
                e.take("host.org_id");
                e.take("host.environment");
                e.set("host.owner_uuid", json!(uuid));
                (o.id.clone(), uuid.to_owned(), uuid.to_owned())
            }
            "grant" => {
                let cid = text(v, "connection_id")?;
                let (owner, ..) = self.connections.get(&cid)?;
                let uuid = self.mapping.uuid(&text(v, "grant.principal_id")?)?;
                e.set("account_uuid", json!(uuid));
                e.set(
                    "created_at",
                    get_path(v, "grant.created_at").cloned().unwrap_or(json!(0)),
                );
                e.set("created_by", Value::Null);
                e.take("grant");
                (grant_key(&cid, uuid), owner.clone(), owner.clone())
            }
            "policy" => {
                let cid = text(v, "connection_id")?;
                let (owner, ..) = self.connections.get(&cid)?;
                let tool = text(v, "policy.tool")?;
                let account = match text(v, "policy.principal_id") {
                    Some(principal) => Some(self.mapping.uuid(&principal)?.to_owned()),
                    None => None,
                };
                e.set("tool", json!(tool));
                e.set("account_uuid", json!(account));
                e.set(
                    "enabled",
                    get_path(v, "policy.enabled")
                        .cloned()
                        .unwrap_or(json!(false)),
                );
                e.take("policy");
                (
                    policy_key(&cid, &tool, account.as_deref()),
                    owner.clone(),
                    owner.clone(),
                )
            }
            "credential" => {
                let cid = text(v, "connection_id")?;
                let (owner, ..) = self.connections.get(&cid)?;
                let uuid = self.mapping.uuid(&text(v, "owner_id")?)?;
                e.take("owner_id");
                e.take("owner_org");
                e.set("owner_uuid", json!(uuid));
                (credential_key(&cid, uuid), owner.clone(), uuid.to_owned())
            }
            "call" => {
                let uuid = self.mapping.uuid(&text(v, "invocation.actor_id")?)?;
                let auth_none = text(v, "invocation.connection_id")
                    .and_then(|cid| self.connections.get(&cid))
                    .is_some_and(|(.., auth)| auth == "none");
                let execution = text(v, "invocation.execution_account_id")
                    .filter(|_| !auth_none)
                    .and_then(|principal| self.mapping.uuid(&principal).map(str::to_owned));
                for path in [
                    "invocation.actor_id",
                    "invocation.execution_account_id",
                    "environment",
                    "org_id",
                    "family",
                    "generation",
                ] {
                    e.take(path);
                }
                e.set("caller_uuid", json!(uuid));
                e.set("execution_account_uuid", json!(execution));
                (o.id.clone(), uuid.to_owned(), uuid.to_owned())
            }
            "settings" => {
                let uuid = self.mapping.uuid(&o.owner)?;
                (
                    crate::operations::settings_key(uuid),
                    uuid.to_owned(),
                    uuid.to_owned(),
                )
            }
            "report" => {
                let principal = text(v, "owner_id").unwrap_or_else(|| o.owner.clone());
                let uuid = self.mapping.uuid(&principal)?;
                let reporter = self
                    .known
                    .get(uuid)
                    .map(|row| row.id.clone())
                    .filter(|id| !id.is_empty())
                    .or_else(|| {
                        self.mapping
                            .links
                            .get(&principal)
                            .map(|l| l.public_id.clone())
                    })
                    .unwrap_or_default();
                e.take("owner_id");
                e.take("org_id");
                e.take("environment");
                e.set("owner_uuid", json!(uuid));
                e.set("reporter_id", json!(reporter));
                (o.id.clone(), uuid.to_owned(), uuid.to_owned())
            }
            "directory" => {
                let uuid = self
                    .mapping
                    .uuid(&text(v, "owner_id").unwrap_or_else(|| o.owner.clone()))?;
                e.take("owner_id");
                e.take("org_id");
                e.take("environment");
                e.set("owner_uuid", json!(uuid));
                if text(v, "source").as_deref() == Some("org") {
                    e.set("source", json!("personal"));
                }
                (o.id.clone(), uuid.to_owned(), uuid.to_owned())
            }
            _ => return None,
        };
        Some(Target {
            id,
            org,
            owner,
            name: o.name.clone(),
            value: e.finish(&o.id, &o.org, &o.owner),
            linked: true,
        })
    }
    fn household(&self, uuid: &str) -> Option<String> {
        self.known
            .get(uuid)
            .and_then(|row| row.household().map(str::to_owned))
    }
}
fn name_path(kind: &str) -> &'static str {
    if kind == "host" { "host.name" } else { "name" }
}
fn candidate_name(kind: &str, desired: &str, attempt: usize) -> String {
    if attempt == 0 {
        return desired.to_owned();
    }
    if kind == "directory" {
        format!("{desired} ({})", attempt + 1)
    } else {
        let suffix = format!("-{}", attempt + 1);
        let mut base = desired.to_owned();
        base.truncate(80 - suffix.len());
        format!("{base}{suffix}")
    }
}
fn bump(report: &mut Map<String, Value>, kind: &str, outcome: &str) {
    let entry = report
        .entry(kind.to_owned())
        .or_insert_with(|| json!({"linked":0,"already_linked":0,"restored":0,"unmapped":0}));
    entry[outcome] = json!(entry[outcome].as_i64().unwrap_or(0) + 1);
}
/// The principal a record belongs to, from its original value.
fn principal_of(kind: &str, o: &Original) -> String {
    let path = match kind {
        "host" => "host.owner_id",
        "grant" => "grant.principal_id",
        "policy" => "policy.principal_id",
        "call" => "invocation.actor_id",
        "settings" => "",
        _ => "owner_id",
    };
    text(&o.value, path).unwrap_or_else(|| o.owner.clone())
}

/// Re-key every production record per `mapping` in one transaction (rolled back
/// unless `commit`). Returns the report printed by `link-identities`.
pub fn link(store: &Store, mapping: &Mapping, known: &Known, commit: bool) -> Result<Value> {
    store.raw_transaction(commit, |tx| link_in(tx, mapping, known, commit))
}
fn link_in(tx: &RawTx<'_, '_>, mapping: &Mapping, known: &Known, commit: bool) -> Result<Value> {
    let uuid_cutovers: i64 =
        tx.connection()
            .query_row("SELECT count(*) FROM account_uuid_migrations", [], |r| {
                r.get(0)
            })?;
    if uuid_cutovers > 0 {
        return Err(Error::bad(
            "Accounts UUID backfill is already applied; legacy identity imports cannot be replayed after it",
        ));
    }

    // Aliases of one legacy identity may agree; different identities must never
    // be folded into one account silently, even when that account has no data.
    let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
    for link in mapping.links.values() {
        let old_id = link.public_id.as_str();
        if let Some(previous) = owners.insert(&link.uuid, old_id)
            && previous != old_id
        {
            return Err(Error::bad(format!(
                "Account {} is mapped from different legacy identities ({previous} and {old_id}); nothing changed.",
                link.uuid
            )));
        }
        if let Some(account) = known.get(&link.uuid) {
            let expected = if old_id.starts_with("si:") {
                Some("silicon")
            } else if old_id.starts_with("c:") {
                Some("carbon")
            } else {
                None
            };
            if expected.is_some_and(|kind| kind != account.kind) {
                return Err(Error::bad(format!(
                    "Legacy identity {old_id} and account {} have different kinds; nothing changed.",
                    link.uuid
                )));
            }
        }
    }
    let source = format!("mapping:{}", &mapping.sha256[..12]);
    let private_reset = tx.migrate_private_visibility()?;
    // Grants this command created in an earlier run are recomputed from scratch.
    let mut previous_cutover = 0;
    let mut records = Vec::new();
    for record in tx.records(&KINDS)? {
        if record.kind == "grant" && record.value.get("cutover").is_some() {
            tx.delete("grant", &record.id)?;
            previous_cutover += 1;
        } else {
            records.push(record);
        }
    }
    let other_environments = records.iter().filter(|r| r.environment != ENV).count();
    records.retain(|r| r.environment == ENV);
    let items: Vec<(RawRecord, Original)> = records
        .into_iter()
        .map(|record| {
            let original = original(&record);
            (record, original)
        })
        .collect();
    let mut plan = Plan {
        mapping,
        known,
        connections: BTreeMap::new(),
    };
    for (record, o) in items.iter().filter(|(r, _)| r.kind == "connection") {
        let principal = text(&o.value, "owner_id").unwrap_or_else(|| o.owner.clone());
        if let Some(uuid) = mapping.uuid(&principal) {
            plan.connections.insert(
                record.id.clone(),
                (
                    uuid.to_owned(),
                    principal,
                    text(&o.value, "visibility").unwrap_or_default(),
                    text(&o.value, "auth_mode").unwrap_or_default(),
                ),
            );
        }
    }
    let mut counts = Map::new();
    let mut unmapped: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut renamed = Vec::new();
    let mut duplicates = Vec::new();
    // Phase 1: decide, and park records that move so moves cannot collide.
    let mut moving = Vec::new();
    for (record, o) in &items {
        let target = plan
            .forward(&record.kind, o)
            .unwrap_or_else(|| Target::restore(o));
        let legacy_now = record.legacy_owner_id.is_some();
        let unchanged = target.id == record.id
            && target.org == record.org_id
            && target.owner == record.owner_id
            && target.name == record.name
            && target.value == record.value
            && target.linked == legacy_now;
        if unchanged {
            bump(
                &mut counts,
                &record.kind,
                if target.linked {
                    "already_linked"
                } else {
                    "unmapped"
                },
            );
            if !target.linked {
                *unmapped
                    .entry(principal_of(&record.kind, o))
                    .or_default()
                    .entry(record.kind.clone())
                    .or_default() += 1;
            }
            continue;
        }
        let parked_id = DERIVED
            .contains(&record.kind.as_str())
            .then(|| format!("relink:{}", record.rowid));
        tx.park(&record.kind, &record.id, parked_id.as_deref())?;
        moving.push((
            record,
            o,
            target,
            parked_id.unwrap_or_else(|| record.id.clone()),
        ));
    }
    // Phase 2: write final ids, owners, names and values (creation order).
    for (record, o, mut target, current) in moving {
        let named = NAMED.contains(&record.kind.as_str());
        let mut attempt = 0;
        let written = loop {
            let name = target
                .name
                .as_deref()
                .filter(|_| named)
                .map(|desired| candidate_name(&record.kind, desired, attempt));
            let mut value = target.value.clone();
            if let (Some(name), Some(desired)) = (&name, &target.name)
                && name != desired
            {
                if let Some(legacy) = value.get_mut("legacy").and_then(|l| l.get_mut("fields"))
                    && let Some(fields) = legacy.as_object_mut()
                {
                    fields
                        .entry(name_path(&record.kind))
                        .or_insert_with(|| json!(desired));
                }
                if let Some((parent, key)) = parent_mut(&mut value, name_path(&record.kind)) {
                    parent.insert(key, json!(name));
                }
            }
            let legacy = target
                .linked
                .then_some((o.id.as_str(), o.org.as_str(), o.owner.as_str()));
            let name = if named { name } else { target.name.clone() };
            if tx.rewrite(
                &record.kind,
                &current,
                &target.id,
                &target.org,
                &target.owner,
                name.as_deref(),
                &value,
                legacy,
            )? {
                if named && attempt > 0 {
                    renamed.push(
                        json!({"kind":record.kind,"id":target.id,"from":target.name,"to":name}),
                    );
                }
                break true;
            }
            if named && target.linked && attempt < 50 {
                attempt += 1;
                continue;
            }
            break false;
        };
        if !written {
            // Two originals map to one record (e.g. two principals linked to one
            // uuid): keep this one as it was originally and report it.
            duplicates.push(json!({"kind":record.kind,"original_id":o.id,"principal":principal_of(&record.kind, o)}));
            target = Target::restore(o);
            if !tx.rewrite(
                &record.kind,
                &current,
                &target.id,
                &target.org,
                &target.owner,
                target.name.as_deref(),
                &target.value,
                None,
            )? {
                return Err(crate::error::Error::internal());
            }
        }
        let outcome = match (target.linked, record.legacy_owner_id.is_some()) {
            (true, false) => "linked",
            (true, true) => "already_linked",
            (false, true) => "restored",
            (false, false) => "unmapped",
        };
        bump(&mut counts, &record.kind, outcome);
        if !target.linked {
            *unmapped
                .entry(principal_of(&record.kind, o))
                .or_default()
                .entry(record.kind.clone())
                .or_default() += 1;
        }
    }
    // Connections that were visible to a whole org become `circle`; principals
    // outside the owner's circle that used one keep it through an explicit grant.
    let mut circle = Vec::new();
    let mut cutover = Vec::new();
    let mut evidence_unmapped = Vec::new();
    for (cid, (owner, owner_principal, visibility, _)) in &plan.connections {
        if visibility != "org" {
            continue;
        }
        circle.push(json!({"connection": cid, "owner_uuid": owner}));
        let mut evidence: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for (record, o) in &items {
            let v = &o.value;
            let (connection, principal, kind) = match record.kind.as_str() {
                "call" => (
                    text(v, "invocation.connection_id"),
                    text(v, "invocation.actor_id"),
                    "call",
                ),
                "credential" => (
                    text(v, "connection_id"),
                    text(v, "owner_id"),
                    "personal provider account",
                ),
                "policy" => (
                    text(v, "connection_id"),
                    text(v, "policy.principal_id"),
                    "tool policy",
                ),
                _ => continue,
            };
            if connection.as_deref() == Some(cid.as_str())
                && let Some(principal) = principal
                && &principal != owner_principal
            {
                evidence.entry(principal).or_default().insert(kind);
            }
        }
        for (principal, kinds) in evidence {
            let Some(uuid) = mapping.uuid(&principal) else {
                evidence_unmapped
                    .push(json!({"connection": cid, "principal": principal, "evidence": kinds}));
                continue;
            };
            let in_circle = uuid == owner
                || plan.household(uuid).is_some() && plan.household(uuid) == plan.household(owner);
            if in_circle {
                continue;
            }
            let value = json!({"connection_id": cid, "account_uuid": uuid, "created_at": now(), "created_by": null,
                "cutover": {"principal_id": principal, "evidence": kinds, "source": source}});
            if tx.insert("grant", &grant_key(cid, uuid), owner, owner, &value)? {
                cutover.push(json!({"connection": cid, "account_uuid": uuid, "principal": principal, "evidence": kinds}));
            }
        }
    }
    // The mapping itself, and what Accounts said about each account.
    let db = tx.connection();
    let linked_at = now();
    let principals: Vec<&String> = mapping.links.keys().collect();
    let mut stale = db.prepare("SELECT iam_principal_id FROM identity_links")?;
    let existing = stale
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stale);
    for principal in existing.iter().filter(|p| !principals.contains(p)) {
        db.execute(
            "DELETE FROM identity_links WHERE iam_principal_id=?",
            [principal],
        )?;
    }
    for link in mapping.links.values() {
        db.execute(
            "INSERT OR REPLACE INTO identity_links(iam_principal_id,iam_public_id,accounts_uuid,linked_at,source) VALUES(?,?,?,?,?)",
            rusqlite::params![link.principal, link.public_id, link.uuid, linked_at, source],
        )?;
    }
    for row in known.values() {
        let mut stored =
            identity_store::read_account(db, &row.uuid)?.unwrap_or_else(|| row.clone());
        if stored.looked_up_at <= row.looked_up_at {
            stored.kind.clone_from(&row.kind);
            stored.id.clone_from(&row.id);
            stored.display_name.clone_from(&row.display_name);
            stored.pfp_url.clone_from(&row.pfp_url);
            stored.custodian_uuid.clone_from(&row.custodian_uuid);
            stored.custodian_id.clone_from(&row.custodian_id);
            stored.looked_up_at = row.looked_up_at;
            stored.synced_at_ms = stored.synced_at_ms.max(row.synced_at_ms);
            if row.status == "deleted" {
                stored.status = "deleted".into();
            }
        }
        stored.updated_at = now();
        identity_store::write_account(db, &stored)?;
    }
    let inert = tx
        .counts()?
        .into_iter()
        .filter(|(kind, environment, _)| !KINDS.contains(&kind.as_str()) || environment != ENV)
        .filter(|(kind, _, _)| kind != "catalog")
        .map(|(kind, environment, count)| json!({"kind": kind, "environment": environment, "records": count}))
        .collect::<Vec<_>>();
    let report = json!({
        "committed": commit,
        "mapping": {"entries": mapping.links.len(), "sha256": mapping.sha256, "source": source},
        "private_connections_reset": private_reset,
        "records": counts,
        "unmapped_principals": unmapped,
        "renamed": renamed,
        "duplicates_kept_unlinked": duplicates,
        "circle_connections": circle,
        "cutover_grants": cutover,
        "cutover_grants_replaced": previous_cutover,
        "evidence_without_mapping": evidence_unmapped,
        "identity_records_in_other_environments": other_environments,
        "left_untouched": inert,
    });
    db.execute(
        "INSERT INTO identity_link_runs(started_at,mapping_sha256,dry_run,report) VALUES(?,?,?,?)",
        rusqlite::params![now(), mapping.sha256, !commit, report.to_string()],
    )?;
    Ok(report)
}

/// Accounts known offline: the kind from the id prefix, no custodian.
pub fn offline_known(mapping: &Mapping) -> Known {
    mapping
        .links
        .values()
        .map(|link| {
            let kind = if link.public_id.starts_with("si:") || link.principal.starts_with("si:") {
                "silicon"
            } else {
                "carbon"
            };
            let mut row = AccountRow::new(&link.uuid, kind);
            row.id = link.public_id.clone();
            (link.uuid.clone(), row)
        })
        .collect()
}
async fn looked_up(app: &App, mapping: &Mapping) -> anyhow::Result<Known> {
    let mut known = Known::new();
    let mut missing = Vec::new();
    for uuid in mapping
        .links
        .values()
        .map(|l| l.uuid.clone())
        .collect::<BTreeSet<_>>()
    {
        match app.accounts.lookup(&uuid, true).await {
            Ok(Some(summary)) => {
                let mut row = AccountRow::new(&uuid, summary.kind.as_str());
                row.apply_summary(&summary, now() * 1000);
                known.insert(uuid, row);
            }
            Ok(None) => anyhow::bail!(
                "Silicon Accounts lookups are over budget; wait a minute and run again."
            ),
            Err(error) if error.1.code == "unknown_account" => missing.push(uuid),
            Err(error) => anyhow::bail!(
                "{} {}",
                error.1.message,
                error.1.recovery.unwrap_or_default()
            ),
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "These accounts_uuid values are not Silicon Accounts accounts: {}. Correct the mapping file (uuids are case-sensitive).",
            missing.join(", ")
        );
    }
    Ok(known)
}
fn require_existing_store(config: &Config) -> anyhow::Result<()> {
    for name in ["mcport.sqlite", "master.key"] {
        anyhow::ensure!(
            config.data_dir.join(name).is_file(),
            "MCPORT_DATA_DIR must name an existing MCPort store with {name}: {}. Nothing was created.",
            config.data_dir.display()
        );
    }
    Ok(())
}

/// `mcport-server link-identities`.
pub async fn run(config: Config, file: &Path, dry_run: bool, offline: bool) -> anyhow::Result<()> {
    require_existing_store(&config)?;
    let text = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("Could not read the mapping file {}: {e}", file.display()))?;
    let mapping = parse_mapping(&text).map_err(|message| anyhow::anyhow!(message))?;
    let app = App::new(config).map_err(|e| anyhow::anyhow!(e.1.message))?;
    let known = if offline {
        offline_known(&mapping)
    } else {
        looked_up(&app, &mapping).await?
    };
    let report =
        link(&app.store, &mapping, &known, !dry_run).map_err(|e| anyhow::anyhow!(e.1.message))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Every pre-0.3.0 principal id production records belong to (or name).
pub fn principals(store: &Store) -> Result<Value> {
    store.raw_transaction(false, |tx| {
        let mut principals: BTreeMap<String, (BTreeSet<String>, BTreeMap<String, i64>)> =
            BTreeMap::new();
        for record in tx.records(&KINDS)? {
            if record.environment != ENV || record.value.get("cutover").is_some() {
                continue;
            }
            let o = original(&record);
            let mut names = vec![(principal_of(&record.kind, &o), o.org.clone())];
            if record.kind == "call"
                && let Some(execution) = text(&o.value, "invocation.execution_account_id")
            {
                names.push((execution, o.org.clone()));
            }
            for (principal, org) in names {
                // IAM-era ids are `c:`/`si:` ids; Accounts uuids never contain ':'.
                if !principal.contains(':') {
                    continue;
                }
                let entry = principals.entry(principal).or_default();
                if !org.is_empty() {
                    entry.0.insert(org);
                }
                *entry.1.entry(record.kind.clone()).or_default() += 1;
            }
        }
        let links: HashMap<String, String> = {
            let db = tx.connection();
            let mut statement =
                db.prepare("SELECT iam_principal_id,accounts_uuid FROM identity_links")?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        Ok(
            json!({"principals": principals.into_iter().map(|(principal, (orgs, records))| json!({
            "iam_principal_id": principal,
            "groups": orgs,
            "records": records,
            "linked_uuid": links.get(&principal),
        })).collect::<Vec<_>>(),
        "mapping_header": "iam_principal_id,accounts_uuid"}),
        )
    })
}
/// `mcport-server legacy-principals`.
pub fn print_principals(config: Config) -> anyhow::Result<()> {
    require_existing_store(&config)?;
    let app = App::new(config).map_err(|e| anyhow::anyhow!(e.1.message))?;
    let value = principals(&app.store).map_err(|e| anyhow::anyhow!(e.1.message))?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        connections::{ConnectionRecord, GrantRecord, PolicyRecord, ProviderGrant},
        execution::CallRecord,
        hosts::HostRecord,
    };

    /// origin/main's schema (before Silicon Accounts), verbatim.
    const ORIGIN_MAIN_SCHEMA: &str = "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,environment TEXT NOT NULL,org_id TEXT NOT NULL,owner_id TEXT NOT NULL,name TEXT,revision INTEGER NOT NULL DEFAULT 1,value TEXT NOT NULL,PRIMARY KEY(kind,id),UNIQUE(kind,environment,org_id,name));
        CREATE INDEX IF NOT EXISTS tenant_records ON records(kind,environment,org_id,owner_id);
        CREATE TABLE IF NOT EXISTS public_id_allocator(singleton INTEGER PRIMARY KEY CHECK(singleton=1),cursor TEXT NOT NULL,seed BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS public_ids(id TEXT PRIMARY KEY NOT NULL);
        CREATE TABLE IF NOT EXISTS public_id_replays(kind TEXT NOT NULL,replay_key TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(kind,replay_key),FOREIGN KEY(kind,id) REFERENCES records(kind,id) ON DELETE CASCADE);";
    const KEY: [u8; 32] = [7; 32];
    const TEST_ENV: &str = "11111111-1111-4111-8111-111111111111";

    fn schema_of(path: &Path) -> (i64, Vec<String>, Vec<String>) {
        let db = rusqlite::Connection::open(path).unwrap();
        let version = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        let mut statement = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables = statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let mut statement = db
            .prepare("SELECT name FROM pragma_table_info('records') ORDER BY cid")
            .unwrap();
        let columns = statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        (version, tables, columns)
    }

    #[test]
    fn schema_migrates_an_empty_and_an_origin_main_database_without_touching_rows() {
        let dir = tempfile::tempdir().unwrap();
        // (a) empty database
        let empty = dir.path().join("empty.sqlite");
        drop(Store::open(&empty, &KEY).unwrap());
        let (version, tables, columns) = schema_of(&empty);
        assert_eq!(version, crate::store::SCHEMA_VERSION);
        for table in [
            "accounts",
            "accounts_webhook_events",
            "identity_link_runs",
            "identity_links",
            "records",
            "silicon_allowances",
        ] {
            assert!(tables.iter().any(|t| t == table), "{table}");
        }
        for column in ["legacy_id", "legacy_org_id", "legacy_owner_id"] {
            assert!(columns.iter().any(|c| c == column), "{column}");
        }
        drop(Store::open(&empty, &KEY).unwrap());
        assert_eq!(
            schema_of(&empty).0,
            crate::store::SCHEMA_VERSION,
            "re-opening is a no-op"
        );
        // (b) a database created by origin/main with a record in it
        let old = dir.path().join("old.sqlite");
        let scratch = Store::open(&dir.path().join("scratch.sqlite"), &KEY).unwrap();
        let value = json!({"id":"aB0","name":"docs","owner_id":"c:saket","org_id":"tos","environment":"production","visibility":"org"});
        let cipher = scratch
            .encrypt(
                &value,
                &Store::aad("connection", "aB0", ENV, "tos", "c:saket"),
            )
            .unwrap();
        {
            let db = rusqlite::Connection::open(&old).unwrap();
            db.execute_batch(ORIGIN_MAIN_SCHEMA).unwrap();
            db.execute(
                "INSERT INTO records(kind,id,environment,org_id,owner_id,name,value) VALUES('connection','aB0','production','tos','c:saket','docs',?)",
                [cipher],
            )
            .unwrap();
        }
        assert_eq!(schema_of(&old).0, 0);
        let store = Store::open(&old, &KEY).unwrap();
        assert_eq!(
            store.get::<Value>("connection", "aB0").unwrap().unwrap(),
            value
        );
        drop(store);
        let (version, tables, _) = schema_of(&old);
        assert_eq!(version, crate::store::SCHEMA_VERSION);
        assert!(tables.iter().any(|t| t == "identity_links"));
    }

    #[allow(clippy::too_many_arguments)] // Mirrors Store::put.
    fn put(
        store: &Store,
        kind: &str,
        id: &str,
        env: &str,
        org: &str,
        owner: &str,
        name: Option<&str>,
        value: Value,
    ) {
        store
            .put(kind, id, env, org, owner, name, &value, None)
            .unwrap();
    }
    fn old_grant(cid: &str, principal: &str) -> String {
        hash(&json!([cid, principal]).to_string())
    }
    fn old_credential(cid: &str, owner: &str, org: &str) -> String {
        hash(&json!([cid, owner, org]).to_string())
    }
    fn connection(
        id: &str,
        name: &str,
        owner: &str,
        org: &str,
        visibility: &str,
        auth: &str,
    ) -> Value {
        json!({"id":id,"name":name,"description":"","org_id":org,"owner_id":owner,"environment":ENV,"transport":"http","url":"https://mcp.example/mcp",
            "host_id":null,"command":null,"args":[],"auth_mode":auth,"visibility":visibility,"status":"ready","can_manage":true,"account":null,"created_at":1,"updated_at":1,"version":1})
    }
    fn call(id: &str, cid: &str, caller: &str, execution: &str) -> Value {
        json!({"invocation":{"id":id,"connection_id":cid,"connection_name":"x","actor_id":caller,"execution_account_id":execution,"method":"tools/call","tool_name":"y",
            "status":"completed","created_at":1,"completed_at":2,"result":{"content":[]},"error":null},
            "environment":ENV,"org_id":"tos","family":"fam","generation":0,"host_id":null,"params":{},"timeout_ms":1000,"expires_at":3,"connection_version":1,"fingerprint":id,"progress":null,"telemetry_enabled":true})
    }
    /// A production store as releases before 0.3.0 left it.
    fn legacy_store(path: &Path) -> Store {
        let s = Store::open(path, &KEY).unwrap();
        put(
            &s,
            "connection",
            "cA1",
            ENV,
            "tos",
            "c:saket",
            Some("github"),
            connection("cA1", "github", "c:saket", "tos", "org", "per-user"),
        );
        put(
            &s,
            "connection",
            "cB1",
            ENV,
            "tos",
            "c:saket",
            Some("docs"),
            connection("cB1", "docs", "c:saket", "tos", "invited", "shared"),
        );
        put(
            &s,
            "connection",
            "cC1",
            ENV,
            "tos",
            "si:stray",
            Some("notes"),
            connection("cC1", "notes", "si:stray", "tos", "invited", "none"),
        );
        put(
            &s,
            "connection",
            "cD1",
            ENV,
            "other",
            "c:saket",
            Some("github"),
            connection("cD1", "github", "c:saket", "other", "invited", "none"),
        );
        put(
            &s,
            "connection",
            "cT1",
            TEST_ENV,
            "tos",
            "c:saket",
            Some("github"),
            connection("cT1", "github", "c:saket", "tos", "invited", "none"),
        );
        put(
            &s,
            "host",
            "hH1",
            ENV,
            "tos",
            "c:saket",
            Some("mac"),
            json!({"host":{"id":"hH1","name":"mac","owner_id":"c:saket","org_id":"tos","environment":ENV,"online":false,"last_seen":null,"created_at":1},
            "token_hash":"t","generation":0,"last_seen":0,"registered":[],"capabilities":{}}),
        );
        for principal in ["si:scout", "si:stray"] {
            put(
                &s,
                "grant",
                &old_grant("cB1", principal),
                ENV,
                "tos",
                "c:saket",
                None,
                json!({"connection_id":"cB1","grant":{"principal_id":principal,"created_at":5}}),
            );
        }
        put(
            &s,
            "policy",
            &hash(&json!(["cA1", "x", null]).to_string()),
            ENV,
            "tos",
            "c:saket",
            None,
            json!({"connection_id":"cA1","policy":{"tool":"x","principal_id":null,"enabled":false}}),
        );
        put(
            &s,
            "policy",
            &hash(&json!(["cA1", "y", "c:outsider"]).to_string()),
            ENV,
            "tos",
            "c:saket",
            None,
            json!({"connection_id":"cA1","policy":{"tool":"y","principal_id":"c:outsider","enabled":false}}),
        );
        for (owner, org) in [("c:outsider", "other"), ("si:scout", "tos")] {
            put(
                &s,
                "credential",
                &old_credential("cA1", owner, org),
                ENV,
                "tos",
                owner,
                None,
                json!({"connection_id":"cA1","owner_id":owner,"owner_org":org,"label":owner,"kind":"bearer","secret":format!("secret-{owner}"),"header_name":null,"oauth":null}),
            );
        }
        put(
            &s,
            "credential",
            &old_credential("cB1", "c:saket", "tos"),
            ENV,
            "tos",
            "c:saket",
            None,
            json!({"connection_id":"cB1","owner_id":"c:saket","owner_org":"tos","label":"Saket","kind":"bearer","secret":"shared-secret","header_name":null,"oauth":null}),
        );
        for (id, cid, caller, execution) in [
            ("k01", "cA1", "c:outsider", "c:outsider"),
            ("k02", "cA1", "si:scout", "si:scout"),
            ("k03", "cA1", "si:stray", "si:stray"),
            ("k04", "cB1", "c:saket", "c:saket"),
        ] {
            put(
                &s,
                "call",
                id,
                ENV,
                "tos",
                caller,
                None,
                call(id, cid, caller, execution),
            );
        }
        put(
            &s,
            "settings",
            &hash(&json!([ENV, "tos", "c:saket"]).to_string()),
            ENV,
            "tos",
            "c:saket",
            None,
            json!({"telemetry":false}),
        );
        put(
            &s,
            "report",
            "r01",
            ENV,
            "tos",
            "c:saket",
            None,
            json!({"id":"r01","environment":ENV,"org_id":"tos","owner_id":"c:saket","message":"bug","pr":null,"status":"delivery_failed","failure_reason":null,"attempts":0,"next_attempt_at":0,"created_at":1}),
        );
        put(
            &s,
            "directory",
            "d01",
            ENV,
            "tos",
            "c:saket",
            Some("Team docs"),
            json!({"id":"d01","name":"Team docs","description":"","category":"Other","source":"org","source_url":null,"source_revision":null,
            "owner_id":"c:saket","org_id":"tos","environment":ENV,"can_manage":false,"template":null,"version":1,"created_at":1,"updated_at":1}),
        );
        put(
            &s,
            "session",
            "s01",
            ENV,
            "tos",
            "c:saket",
            None,
            json!({"iam_refresh":"kept"}),
        );
        s
    }
    fn known(entries: &[(&str, &str, &str, Option<&str>)]) -> Known {
        entries
            .iter()
            .map(|(uuid, kind, id, custodian)| {
                let mut row = AccountRow::new(uuid, kind);
                row.id = (*id).into();
                row.custodian_uuid = custodian.map(str::to_owned);
                row.looked_up_at = now();
                ((*uuid).to_owned(), row)
            })
            .collect()
    }
    /// Every record as (kind, id) → (columns, value), cutover grants excluded.
    /// (org, owner, name, legacy_id, legacy_owner_id, value) of one record.
    type Row = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Value,
    );
    fn snapshot(store: &Store) -> BTreeMap<(String, String), Row> {
        store
            .raw_transaction(false, |tx| {
                Ok(tx
                    .records(&[
                        "connection",
                        "host",
                        "grant",
                        "policy",
                        "credential",
                        "call",
                        "settings",
                        "report",
                        "directory",
                        "session",
                        "catalog",
                    ])?
                    .into_iter()
                    .filter(|r| r.value.get("cutover").is_none())
                    .map(|r| {
                        (
                            (r.kind, r.id),
                            (
                                r.org_id,
                                r.owner_id,
                                r.name,
                                r.legacy_id,
                                r.legacy_owner_id,
                                r.value,
                            ),
                        )
                    })
                    .collect())
            })
            .unwrap()
    }
    const MAPPING: &str = "# cutover mapping\niam_principal_id,accounts_uuid,iam_public_id\nc:saket,zQo,c:saket\nsi:scout,Sc1,\nc:outsider,Out,c:outsider\n";

    #[test]
    fn mapping_files_are_validated_with_line_numbers() {
        let mapping = parse_mapping(MAPPING).unwrap();
        assert_eq!(mapping.uuid("si:scout"), Some("Sc1"));
        assert_eq!(mapping.links["si:scout"].public_id, "si:scout");
        for (text, message) in [
            ("", "The mapping file is empty"),
            (
                "principal,uuid\n",
                "line 1: the mapping file must start with the header",
            ),
            (
                "iam_principal_id,accounts_uuid\nc:a,zQo,extra\n",
                "line 2: expected 2 comma-separated values",
            ),
            (
                "iam_principal_id,accounts_uuid\nc:a,not-a-uuid\n",
                "line 2: accounts_uuid `not-a-uuid` is not a Silicon Accounts uuid",
            ),
            (
                "iam_principal_id,accounts_uuid\nc:a,zQo\nc:a,zQ2\n",
                "line 3: iam_principal_id `c:a` appears twice",
            ),
        ] {
            let error = parse_mapping(text).unwrap_err();
            assert!(error.starts_with(message), "{error}");
        }
    }

    #[test]
    fn link_identities_rekeys_every_kind_reversibly_and_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let store = legacy_store(&dir.path().join("mcport.sqlite"));
        let mapping = parse_mapping(MAPPING).unwrap();
        let accounts = known(&[
            ("zQo", "carbon", "c:saket", None),
            ("Sc1", "silicon", "si:scout", Some("zQo")),
            ("Out", "carbon", "c:outsider", None),
        ]);
        let before = snapshot(&store);

        // A dry run reports and changes nothing.
        let report = link(&store, &mapping, &accounts, false).unwrap();
        assert_eq!(report["committed"], false);
        assert_eq!(report["records"]["connection"]["linked"], 3);
        assert_eq!(snapshot(&store), before);

        let report = link(&store, &mapping, &accounts, true).unwrap();
        assert_eq!(
            report["records"]["connection"],
            json!({"linked":3,"already_linked":0,"restored":0,"unmapped":1})
        );
        assert_eq!(report["records"]["call"]["linked"], 3);
        assert_eq!(
            report["records"]["grant"],
            json!({"linked":1,"already_linked":0,"restored":0,"unmapped":1})
        );
        assert_eq!(report["records"]["policy"]["linked"], 2);
        assert_eq!(report["records"]["credential"]["linked"], 3);
        assert_eq!(report["identity_records_in_other_environments"], 1);
        assert_eq!(report["unmapped_principals"]["si:stray"]["connection"], 1);
        assert_eq!(report["renamed"][0]["to"], "github-2");
        assert_eq!(
            report["circle_connections"],
            json!([{"connection":"cA1","owner_uuid":"zQo"}])
        );
        // The outsider used the org connection (call, personal account, policy):
        // it keeps it through a grant. The Silicon in the owner's circle needs none.
        assert_eq!(report["cutover_grants"].as_array().unwrap().len(), 1);
        assert_eq!(report["cutover_grants"][0]["account_uuid"], "Out");
        assert_eq!(
            report["evidence_without_mapping"][0]["principal"],
            "si:stray"
        );

        // Typed reads see the Accounts-era shape; AAD and derived ids line up.
        let a = store
            .get::<ConnectionRecord>("connection", "cA1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (a.owner_uuid.as_str(), a.visibility.as_str()),
            ("zQo", "circle")
        );
        let d = store
            .get::<ConnectionRecord>("connection", "cD1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (d.owner_uuid.as_str(), d.name.as_str()),
            ("zQo", "github-2")
        );
        assert!(
            store
                .get::<ConnectionRecord>("connection", "cC1")
                .unwrap()
                .unwrap()
                .owner_uuid
                .is_empty()
        );
        assert!(
            store
                .get::<ConnectionRecord>("connection", "cT1")
                .unwrap()
                .unwrap()
                .owner_uuid
                .is_empty(),
            "test worlds stay"
        );
        let grant = store
            .get::<GrantRecord>("grant", &grant_key("cB1", "Sc1"))
            .unwrap()
            .unwrap();
        assert_eq!((grant.account_uuid.as_str(), grant.created_at), ("Sc1", 5));
        assert!(
            store
                .get::<Value>("grant", &old_grant("cB1", "si:stray"))
                .unwrap()
                .is_some()
        );
        let cutover = store
            .get::<GrantRecord>("grant", &grant_key("cA1", "Out"))
            .unwrap()
            .unwrap();
        assert_eq!(cutover.account_uuid, "Out");
        assert!(
            store
                .get::<GrantRecord>("grant", &grant_key("cA1", "Sc1"))
                .unwrap()
                .is_none()
        );
        let global = store
            .get::<PolicyRecord>("policy", &policy_key("cA1", "x", None))
            .unwrap()
            .unwrap();
        assert!(!global.enabled && global.account_uuid.is_none());
        assert!(
            store
                .get::<PolicyRecord>("policy", &policy_key("cA1", "y", Some("Out")))
                .unwrap()
                .is_some()
        );
        for (cid, owner, secret) in [
            ("cA1", "Out", "secret-c:outsider"),
            ("cA1", "Sc1", "secret-si:scout"),
            ("cB1", "zQo", "shared-secret"),
        ] {
            let credential = store
                .get::<ProviderGrant>("credential", &credential_key(cid, owner))
                .unwrap()
                .unwrap();
            assert_eq!(
                (credential.owner_uuid.as_str(), credential.secret.as_str()),
                (owner, secret)
            );
        }
        let outsider_call = store.get::<CallRecord>("call", "k01").unwrap().unwrap();
        assert_eq!(outsider_call.caller_uuid, "Out");
        assert_eq!(outsider_call.execution_account_uuid.as_deref(), Some("Out"));
        assert!(
            store
                .get::<CallRecord>("call", "k03")
                .unwrap()
                .unwrap()
                .caller_uuid
                .is_empty()
        );
        let settings: Value = store
            .get("settings", &crate::operations::settings_key("zQo"))
            .unwrap()
            .unwrap();
        assert_eq!(settings["telemetry"], false);
        let report_record: Value = store.get("report", "r01").unwrap().unwrap();
        assert_eq!(
            (
                report_record["owner_uuid"].as_str(),
                report_record["reporter_id"].as_str()
            ),
            (Some("zQo"), Some("c:saket"))
        );
        let entry: Value = store.get("directory", "d01").unwrap().unwrap();
        assert_eq!(
            (entry["source"].as_str(), entry["owner_uuid"].as_str()),
            (Some("personal"), Some("zQo"))
        );
        let host = store.get::<HostRecord>("host", "hH1").unwrap().unwrap();
        assert_eq!(host.host.owner_uuid, "zQo");
        assert_eq!(host.legacy_org().as_deref(), Some("tos"));
        assert!(host.legacy_registry());
        assert!(store.get::<Value>("session", "s01").unwrap().is_some());
        assert_eq!(store.legacy_ids("Sc1").unwrap(), vec!["si:scout"]);
        assert_eq!(
            store
                .account("Sc1")
                .unwrap()
                .unwrap()
                .custodian_uuid
                .as_deref(),
            Some("zQo")
        );
        let linked = snapshot(&store);

        // Re-running the same mapping changes nothing.
        let report = link(&store, &mapping, &accounts, true).unwrap();
        assert_eq!(
            report["records"]["connection"],
            json!({"linked":0,"already_linked":3,"restored":0,"unmapped":1})
        );
        assert_eq!(report["cutover_grants_replaced"], 1);
        assert_eq!(snapshot(&store), linked);

        // Another mapping recomputes from the originals: c:saket moves, c:outsider is left out.
        let other =
            parse_mapping("iam_principal_id,accounts_uuid\nc:saket,zQ2\nsi:scout,Sc1\n").unwrap();
        let other_accounts = known(&[
            ("zQ2", "carbon", "c:saket", None),
            ("Sc1", "silicon", "si:scout", Some("zQ2")),
        ]);
        let report = link(&store, &other, &other_accounts, true).unwrap();
        assert_eq!(report["records"]["policy"]["restored"], 1);
        assert_eq!(
            store
                .get::<ConnectionRecord>("connection", "cA1")
                .unwrap()
                .unwrap()
                .owner_uuid,
            "zQ2"
        );
        assert!(
            store
                .get::<GrantRecord>("grant", &grant_key("cA1", "Out"))
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get::<Value>("credential", &old_credential("cA1", "c:outsider", "other"))
                .unwrap()
                .is_some(),
            "restored under its original id"
        );
        assert!(
            store
                .get::<CallRecord>("call", "k01")
                .unwrap()
                .unwrap()
                .caller_uuid
                .is_empty()
        );
        assert!(store.legacy_ids("Out").unwrap().is_empty());

        // And back: the first mapping reproduces the first result exactly.
        link(&store, &mapping, &accounts, true).unwrap();
        assert_eq!(snapshot(&store), linked);
        // Every record still decrypts under its current columns.
        for kind in KINDS {
            store.list::<Value>(kind, None).unwrap();
        }
        assert_eq!(
            principals(&store).unwrap()["principals"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["linked_uuid"].is_null())
                .count(),
            1
        );
    }

    #[test]
    fn different_principals_cannot_be_mapped_to_one_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("mcport.sqlite"), &KEY).unwrap();
        for principal in ["c:saket", "c:saket-old"] {
            put(
                &store,
                "settings",
                &hash(&json!([ENV, "tos", principal]).to_string()),
                ENV,
                "tos",
                principal,
                None,
                json!({"telemetry":principal == "c:saket"}),
            );
        }
        let mapping =
            parse_mapping("iam_principal_id,accounts_uuid\nc:saket,zQo\nc:saket-old,zQo\n")
                .unwrap();
        let before = snapshot(&store);
        let error = link(
            &store,
            &mapping,
            &known(&[("zQo", "carbon", "c:saket", None)]),
            true,
        )
        .unwrap_err();
        assert!(error.1.message.contains("different legacy identities"));
        assert_eq!(
            snapshot(&store),
            before,
            "the refused mapping changes nothing"
        );
        let mismatch = parse_mapping("iam_principal_id,accounts_uuid\nc:saket,zQo\n").unwrap();
        let error = link(
            &store,
            &mismatch,
            &known(&[("zQo", "silicon", "si:other", None)]),
            true,
        )
        .unwrap_err();
        assert!(error.1.message.contains("different kinds"));
        assert_eq!(snapshot(&store), before);
    }
}
