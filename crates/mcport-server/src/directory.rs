//! Discovery metadata only: no network requests, credentials, or executable actions.
//!
//! Personal entries belong to their creator; the custodian of a Silicon owner can
//! see and manage them, and the owner (or custodian) can share an entry with
//! other accounts. The bundled community catalog is readable by every signed-in
//! Carbon and Silicon.
use crate::{
    accounts::{self, AccountRow},
    auth::{Auth, Live},
    error::{Error, Result},
    state::{App, hash, now},
    store::ENV,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use mcport_core::{
    AccessGrant, AccessInput, DirectoryEntry, DirectoryInput, DirectoryTemplate, DirectoryUpdate,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

const CATALOG_ENV: &str = "catalog";
const MAX_ENTRIES_PER_ACCOUNT: usize = 1000;

/// Stored entry (personal `directory` records and community `catalog` records).
/// `other` keeps fields of earlier releases verbatim.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct DirectoryRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    /// `community` or `personal` (`org` in records written before 0.3.0).
    pub source: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub owner_uuid: String,
    #[serde(default)]
    pub template: Option<DirectoryTemplate>,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
/// Permission to see one personal entry.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct EntryGrant {
    pub entry_id: String,
    pub account_uuid: String,
    pub created_at: i64,
    pub created_by: String,
}
pub fn entry_grant_key(entry: &str, account: &str) -> String {
    hash(&json!(["directory", entry, account]).to_string())
}

#[derive(Deserialize)]
struct Snapshot {
    repository: String,
    revision: String,
    license: String,
    entries: Vec<DirectoryInput>,
}

fn url(value: &str) -> Result<()> {
    let parsed = url::Url::parse(value)
        .map_err(|_| Error::bad("Directory URLs must be valid HTTP(S) URLs."))?;
    if value.len() > 2048
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || value.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Directory URLs cannot contain credentials, query strings or fragments. Supply provider secrets when configuring the connection.",
        ));
    }
    Ok(())
}
fn validate_template(template: &DirectoryTemplate) -> Result<()> {
    if !matches!(template.auth_mode.as_str(), "none" | "per-user" | "shared") {
        return Err(Error::bad(
            "Choose none, per-user or shared provider authentication.",
        ));
    }
    match template.transport.as_str() {
        "http" => {
            if let Some(value) = &template.url {
                url(value)?;
            }
            if template.command.is_some() || !template.args.is_empty() {
                return Err(Error::bad(
                    "HTTP templates cannot contain a process command or arguments.",
                ));
            }
        }
        "stdio" => {
            if template.url.is_some() {
                return Err(Error::bad("stdio templates cannot contain an HTTP URL."));
            }
            if let Some(command) = &template.command
                && (command.trim().is_empty()
                    || command.len() > 2048
                    || command.chars().any(char::is_control))
            {
                return Err(Error::bad(
                    "Use a nonempty executable name or path without control characters.",
                ));
            }
        }
        _ => return Err(Error::bad("Choose http or stdio transport.")),
    }
    if template.args.len() > 128
        || template
            .args
            .iter()
            .any(|arg| arg.len() > 2048 || arg.chars().any(char::is_control))
    {
        return Err(Error::bad(
            "Template arguments exceed the size limit or contain control characters.",
        ));
    }
    for arg in &template.args {
        let lower = arg.to_ascii_lowercase();
        let flag = lower
            .split('=')
            .next()
            .unwrap_or("")
            .trim_start_matches('-')
            .replace('_', "-");
        if matches!(
            flag.as_str(),
            "token"
                | "api-key"
                | "apikey"
                | "access-token"
                | "password"
                | "secret"
                | "client-secret"
                | "authorization"
        ) || lower.starts_with("bearer ")
        {
            return Err(Error::bad(
                "Do not put credentials in shared directory arguments. Configure them in the connection's provider account.",
            ));
        }
        if lower.starts_with("http://") || lower.starts_with("https://") {
            url(arg)?;
        }
    }
    Ok(())
}
fn validate(mut input: DirectoryInput) -> Result<DirectoryInput> {
    input.name = input.name.trim().to_owned();
    input.category = input.category.trim().to_owned();
    if input.category.is_empty() {
        input.category = "Other".into();
    }
    if input.name.is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
        || input.description.len() > 4096
        || input
            .description
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        || input.category.len() > 64
        || input.category.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Use a name up to 128 bytes, description up to 4096 bytes and category up to 64 bytes without control characters.",
        ));
    }
    if let Some(source) = &input.source_url {
        url(source)?;
    }
    if let Some(template) = &input.template {
        validate_template(template)?;
    }
    Ok(input)
}

fn input_of(entry: &DirectoryRecord) -> DirectoryInput {
    DirectoryInput {
        name: entry.name.clone(),
        description: entry.description.clone(),
        category: entry.category.clone(),
        source_url: entry.source_url.clone(),
        template: entry.template.clone(),
    }
}
fn assign(entry: &mut DirectoryRecord, input: DirectoryInput) {
    entry.name = input.name;
    entry.description = input.description;
    entry.category = input.category;
    entry.source_url = input.source_url;
    entry.template = input.template;
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryAccess {
    Community,
    Owner,
    Custodian,
    Shared,
}
async fn entry_access(
    app: &App,
    entry: &DirectoryRecord,
    who: &AccountRow,
) -> Result<Option<EntryAccess>> {
    if entry.source == "community" {
        return Ok(Some(EntryAccess::Community));
    }
    if entry.owner_uuid.is_empty() || !matches!(entry.source.as_str(), "personal" | "org") {
        return Ok(None);
    }
    if entry.owner_uuid == who.uuid {
        return Ok(Some(EntryAccess::Owner));
    }
    if let Some(owner) = app.store.account(&entry.owner_uuid)? {
        if accounts::looks_after(app, who, &owner).await {
            return Ok(Some(EntryAccess::Custodian));
        }
        if !owner.active() {
            return Ok(None);
        }
    }
    Ok(app
        .store
        .get::<EntryGrant>("directory_grant", &entry_grant_key(&entry.id, &who.uuid))?
        .filter(|grant| grant.account_uuid == who.uuid && grant.entry_id == entry.id)
        .map(|_| EntryAccess::Shared))
}
fn view(app: &App, entry: DirectoryRecord, access: EntryAccess) -> DirectoryEntry {
    DirectoryEntry {
        owner: (!entry.owner_uuid.is_empty()).then(|| accounts::reference(app, &entry.owner_uuid)),
        can_manage: matches!(access, EntryAccess::Owner | EntryAccess::Custodian),
        source: if entry.source == "org" {
            "personal".into()
        } else {
            entry.source
        },
        id: entry.id,
        name: entry.name,
        description: entry.description,
        category: entry.category,
        source_url: entry.source_url,
        source_revision: entry.source_revision,
        template: entry.template,
        version: entry.version,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}
async fn resolve(
    app: &App,
    who: &AccountRow,
    id: &str,
    manage: bool,
) -> Result<(DirectoryRecord, EntryAccess)> {
    let entry = app
        .store
        .get::<DirectoryRecord>("directory", id)?
        .or(app.store.get::<DirectoryRecord>("catalog", id)?)
        .ok_or_else(Error::missing)?;
    let access = entry_access(app, &entry, who)
        .await?
        .ok_or_else(Error::missing)?;
    if manage && !matches!(access, EntryAccess::Owner | EntryAccess::Custodian) {
        return Err(Error::new(
            403,
            "access_denied",
            "Only an entry's creator (or the custodian of a Silicon creator) can change it; community entries are read-only.",
            "Add your own directory entry to use different setup defaults.",
        ));
    }
    Ok((entry, access))
}
async fn list_entries(app: &App, who: &AccountRow, query: &str) -> Result<Vec<DirectoryEntry>> {
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err(Error::bad(
            "Directory search must be at most 256 bytes without control characters.",
        ));
    }
    let query = query.trim().to_lowercase();
    let mut entries = app
        .store
        .list::<DirectoryRecord>("catalog", Some(CATALOG_ENV))?;
    entries.extend(app.store.list::<DirectoryRecord>("directory", Some(ENV))?);
    let mut visible = Vec::new();
    for entry in entries {
        if !(query.is_empty()
            || [
                entry.name.as_str(),
                entry.description.as_str(),
                entry.category.as_str(),
            ]
            .iter()
            .any(|field| field.to_lowercase().contains(&query)))
        {
            continue;
        }
        if let Some(access) = entry_access(app, &entry, who).await? {
            visible.push((entry, access));
        }
    }
    visible.sort_by_cached_key(|(entry, _)| (entry.name.to_lowercase(), entry.id.clone()));
    Ok(visible
        .into_iter()
        .map(|(entry, access)| view(app, entry, access))
        .collect())
}
fn create_entry(app: &App, who: &AccountRow, input: DirectoryInput) -> Result<DirectoryEntry> {
    let input = validate(input)?;
    if app
        .store
        .list::<DirectoryRecord>("directory", Some(ENV))?
        .iter()
        .filter(|entry| entry.owner_uuid == who.uuid)
        .count()
        >= MAX_ENTRIES_PER_ACCOUNT
    {
        return Err(Error::bad(
            "You have reached the limit of 1000 directory entries. Delete entries you no longer need.",
        ));
    }
    let mut entry = DirectoryRecord {
        id: String::new(),
        name: input.name.clone(),
        description: input.description,
        category: input.category,
        source: "personal".into(),
        source_url: input.source_url,
        source_revision: None,
        owner_uuid: who.uuid.clone(),
        template: input.template,
        version: 1,
        created_at: now(),
        updated_at: now(),
        other: Map::new(),
    };
    let name = entry.name.clone();
    let (entry, _) = app.store.create_public(
        "directory",
        ENV,
        &who.uuid,
        &who.uuid,
        Some(&name),
        None,
        |id| {
            entry.id = id;
            entry
        },
    )?;
    Ok(view(app, entry, EntryAccess::Owner))
}
async fn update_entry(
    app: &App,
    who: &AccountRow,
    id: &str,
    update: DirectoryUpdate,
) -> Result<DirectoryEntry> {
    let input = validate(update.input)?;
    let (mut entry, access) = resolve(app, who, id, true).await?;
    if entry.version != update.version {
        return Err(Error::new(
            409,
            "revision_conflict",
            "This directory entry changed.",
            "Refresh it before saving your edit.",
        ));
    }
    assign(&mut entry, input);
    entry.version += 1;
    entry.updated_at = now();
    app.store.put(
        "directory",
        &entry.id,
        ENV,
        &entry.owner_uuid,
        &entry.owner_uuid,
        Some(&entry.name),
        &entry,
        Some(update.version),
    )?;
    Ok(view(app, entry, access))
}
/// Run once before serving. The catalog has no organization or test-environment owner.
pub fn seed(app: &App) -> Result<()> {
    let snapshot: Snapshot = serde_json::from_str(include_str!("../catalog/community.json"))?;
    if snapshot.repository != "https://github.com/wong2/awesome-mcp-servers"
        || snapshot.license != "MIT"
        || snapshot.revision.len() != 40
        || !snapshot.revision.bytes().all(|c| c.is_ascii_hexdigit())
        || snapshot.entries.is_empty()
    {
        return Err(Error::internal());
    }
    // Validate the entire snapshot before changing existing records.
    let inputs = snapshot
        .entries
        .into_iter()
        .map(validate)
        .collect::<Result<Vec<_>>>()?;
    let mut retained = HashSet::new();
    for input in inputs {
        let source = input.source_url.as_ref().ok_or_else(Error::internal)?;
        let replay = hash(&format!("community:{source}"));
        let timestamp = now();
        let (mut entry, created) = app.store.create_public(
            "catalog",
            CATALOG_ENV,
            "",
            "",
            Some(source),
            Some(&replay),
            |id| DirectoryRecord {
                id,
                name: input.name.clone(),
                description: input.description.clone(),
                category: input.category.clone(),
                source: "community".into(),
                source_url: input.source_url.clone(),
                source_revision: Some(snapshot.revision.clone()),
                owner_uuid: String::new(),
                template: input.template.clone(),
                version: 1,
                created_at: timestamp,
                updated_at: timestamp,
                other: Map::new(),
            },
        )?;
        if !created
            && (input_of(&entry) != input
                || entry.source_revision.as_ref() != Some(&snapshot.revision))
        {
            let previous = entry.version;
            assign(&mut entry, input);
            entry.source_revision = Some(snapshot.revision.clone());
            entry.updated_at = timestamp;
            entry.version += 1;
            app.store.put(
                "catalog",
                &entry.id,
                CATALOG_ENV,
                "",
                "",
                entry.source_url.as_deref(),
                &entry,
                Some(previous),
            )?;
        }
        retained.insert(entry.id);
    }
    for entry in app
        .store
        .list::<DirectoryRecord>("catalog", Some(CATALOG_ENV))?
    {
        if !retained.contains(&entry.id) {
            app.store.delete("catalog", &entry.id)?;
        }
    }
    Ok(())
}

#[derive(Default, Deserialize)]
pub struct Search {
    #[serde(default)]
    pub q: String,
}
pub async fn list(
    State(app): State<App>,
    a: Auth,
    Query(search): Query<Search>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"data": list_entries(&app, &a.account, &search.q).await?}),
    ))
}
pub async fn get(State(app): State<App>, a: Auth, Path(id): Path<String>) -> Result<Json<Value>> {
    let (entry, access) = resolve(&app, &a.account, &id, false).await?;
    Ok(Json(json!({"data": view(&app, entry, access)})))
}
pub async fn create(
    State(app): State<App>,
    a: Auth,
    Json(input): Json<DirectoryInput>,
) -> Result<Json<Value>> {
    let lock = app.lock(&format!("directory-owner:{}", a.uuid()));
    let _guard = lock.lock().await;
    Ok(Json(
        json!({"data": create_entry(&app, &a.account, input)?}),
    ))
}
pub async fn update(
    State(app): State<App>,
    a: Auth,
    Path(id): Path<String>,
    Json(input): Json<DirectoryUpdate>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"data": update_entry(&app, &a.account, &id, input).await?}),
    ))
}
pub async fn remove(
    State(app): State<App>,
    a: Auth,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (entry, _) = resolve(&app, &a.account, &id, true).await?;
    app.store.delete("directory", &entry.id)?;
    for grant in app.store.list::<EntryGrant>("directory_grant", None)? {
        if grant.entry_id == entry.id {
            app.store.delete(
                "directory_grant",
                &entry_grant_key(&entry.id, &grant.account_uuid),
            )?;
        }
    }
    Ok(Json(json!({"data":{"deleted":true}})))
}
fn grant_view(app: &App, grant: &EntryGrant) -> AccessGrant {
    AccessGrant {
        account: accounts::reference(app, &grant.account_uuid),
        created_at: grant.created_at,
        created_by: Some(accounts::reference(app, &grant.created_by)),
    }
}
pub async fn access_list(
    State(app): State<App>,
    a: Auth,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (entry, _) = resolve(&app, &a.account, &id, true).await?;
    let out = app
        .store
        .list::<EntryGrant>("directory_grant", None)?
        .into_iter()
        .filter(|grant| grant.entry_id == entry.id)
        .map(|grant| grant_view(&app, &grant))
        .collect::<Vec<_>>();
    Ok(Json(json!({"data": out})))
}
pub async fn share(
    State(app): State<App>,
    Live(a): Live,
    Path(id): Path<String>,
    Json(input): Json<AccessInput>,
) -> Result<Json<Value>> {
    let (entry, _) = resolve(&app, &a.account, &id, true).await?;
    if entry.source == "community" {
        return Err(Error::bad(
            "Community entries are already visible to everyone signed in.",
        ));
    }
    let target = accounts::resolve(&app, a.uuid(), &input.account).await?;
    if target.uuid == entry.owner_uuid {
        return Err(Error::bad("The creator already sees its own entry."));
    }
    crate::connections::ensure_reachable(&app, &a.account, &target).await?;
    let key = entry_grant_key(&entry.id, &target.uuid);
    let grant = match app.store.get::<EntryGrant>("directory_grant", &key)? {
        Some(existing) => existing,
        None => {
            let grant = EntryGrant {
                entry_id: entry.id.clone(),
                account_uuid: target.uuid.clone(),
                created_at: now(),
                created_by: a.uuid().into(),
            };
            app.store.put(
                "directory_grant",
                &key,
                ENV,
                &entry.owner_uuid,
                &entry.owner_uuid,
                None,
                &grant,
                None,
            )?;
            grant
        }
    };
    Ok(Json(json!({"data": grant_view(&app, &grant)})))
}
pub async fn unshare(
    State(app): State<App>,
    Live(a): Live,
    Path((id, account)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let (entry, _) = resolve(&app, &a.account, &id, true).await?;
    let uuid = if app
        .store
        .get::<EntryGrant>("directory_grant", &entry_grant_key(&entry.id, &account))?
        .is_some()
    {
        account
    } else {
        accounts::resolve(&app, a.uuid(), &account).await?.uuid
    };
    app.store
        .delete("directory_grant", &entry_grant_key(&entry.id, &uuid))?;
    Ok(Json(json!({"data":{"deleted":true}})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, fixture};
    use axum::http::StatusCode;
    fn input(name: &str) -> DirectoryInput {
        DirectoryInput {
            name: name.into(),
            description: "A reusable setup, not a live connection".into(),
            category: "".into(),
            source_url: Some("https://provider.example/docs".into()),
            template: Some(DirectoryTemplate {
                transport: "http".into(),
                url: Some("https://mcp.provider.example/mcp".into()),
                command: None,
                args: vec![],
                auth_mode: "per-user".into(),
            }),
        }
    }
    async fn people() -> Fixture {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.silicon("Scout", "si:scout", "Ada");
        f.silicon("Pilot", "si:pilot", "Ada");
        f.carbon("Bob", "c:bob");
        f.silicon("Rover", "si:rover", "Bob");
        f
    }
    #[tokio::test]
    async fn personal_entries_are_for_owner_custodian_and_explicit_shares() {
        let f = people().await;
        let scout = f.auth("Scout").await;
        let ada = f.auth("Ada").await;
        let pilot = f.auth("Pilot").await;
        let bob = f.auth("Bob").await;
        let entry = create_entry(&f.app, &scout.account, input("Payments Directory")).unwrap();
        assert_eq!(entry.id.len(), 3);
        assert_eq!(entry.category, "Other");
        assert_eq!(entry.source, "personal");
        assert!(entry.can_manage);
        assert_eq!(entry.owner.as_ref().unwrap().id, "si:scout");
        // The custodian sees and manages it; the custodian's other Silicon does not see it.
        let (seen, access) = resolve(&f.app, &ada.account, &entry.id, true)
            .await
            .unwrap();
        assert!(access == EntryAccess::Custodian && seen.id == entry.id);
        assert_eq!(
            list_entries(&f.app, &ada.account, "PAYMENTS")
                .await
                .unwrap()
                .len(),
            1
        );
        for other in [&pilot, &bob] {
            assert!(
                list_entries(&f.app, &other.account, "payments")
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                resolve(&f.app, &other.account, &entry.id, false)
                    .await
                    .err()
                    .unwrap()
                    .0,
                StatusCode::NOT_FOUND
            );
        }
        let updated = update_entry(
            &f.app,
            &ada.account,
            &entry.id,
            DirectoryUpdate {
                input: input("Renamed setup"),
                version: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.version, 2);
        let stale = update_entry(
            &f.app,
            &scout.account,
            &entry.id,
            DirectoryUpdate {
                input: input("Stale write"),
                version: 1,
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(stale.1.code, "revision_conflict");
        // Sharing by id makes it visible (read-only) to that account.
        let (status, body) = f
            .as_(
                "Scout",
                "POST",
                &format!("/api/v1/directory/{}/access", entry.id),
                Some(json!({"account":"c:bob"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let shared = list_entries(&f.app, &bob.account, "renamed").await.unwrap();
        assert_eq!(shared.len(), 1);
        assert!(!shared[0].can_manage);
        assert_eq!(
            resolve(&f.app, &bob.account, &entry.id, true)
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::FORBIDDEN
        );
        // Silicons outside the circle need an allowance first.
        let (status, body) = f
            .as_(
                "Scout",
                "POST",
                &format!("/api/v1/directory/{}/access", entry.id),
                Some(json!({"account":"si:rover"})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, _) = f
            .as_(
                "Scout",
                "DELETE",
                &format!("/api/v1/directory/{}/access/c:bob", entry.id),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            list_entries(&f.app, &bob.account, "renamed")
                .await
                .unwrap()
                .is_empty()
        );
        // Nothing here created a connection or credential.
        assert!(
            f.app
                .store
                .list::<Value>("connection", None)
                .unwrap()
                .is_empty()
        );
        assert!(
            f.app
                .store
                .list::<Value>("credential", None)
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn entries_written_before_accounts_read_as_personal_once_linked() {
        let f = people().await;
        let ada = f.auth("Ada").await;
        let legacy = json!({"id":"dR1","name":"Team docs","description":"","category":"Documentation","source":"org","source_url":null,"source_revision":null,"owner_id":"c:ada","org_id":"tos","environment":"production","can_manage":false,"template":null,"version":1,"created_at":1,"updated_at":1});
        f.app
            .store
            .put(
                "directory",
                "dR1",
                ENV,
                "tos",
                "c:ada",
                Some("Team docs"),
                &legacy,
                Some(0),
            )
            .unwrap();
        assert!(
            list_entries(&f.app, &ada.account, "team")
                .await
                .unwrap()
                .is_empty()
        );
        let mut linked = legacy.clone();
        linked["owner_uuid"] = json!("Ada");
        f.app
            .store
            .put(
                "directory",
                "dR1",
                ENV,
                "Ada",
                "Ada",
                Some("Team docs"),
                &linked,
                None,
            )
            .unwrap();
        let entries = list_entries(&f.app, &ada.account, "team").await.unwrap();
        assert_eq!(entries[0].source, "personal");
        assert!(entries[0].can_manage);
    }
    #[tokio::test]
    async fn bundled_catalog_is_licensed_stable_and_read_only_for_everyone() {
        let f = people().await;
        seed(&f.app).unwrap();
        let ada = f.auth("Ada").await;
        let rover = f.auth("Rover").await;
        let builtin = list_entries(&f.app, &ada.account, "").await.unwrap();
        assert!(builtin.len() >= 500);
        let ids: HashSet<_> = builtin.iter().map(|entry| entry.id.clone()).collect();
        assert_eq!(ids.len(), builtin.len());
        assert!(builtin.iter().all(|entry| {
            entry.id.len() == 3
                && !entry.can_manage
                && entry.owner.is_none()
                && entry
                    .source_revision
                    .as_ref()
                    .is_some_and(|revision| revision.len() == 40)
                && entry.source_url.is_some()
        }));
        let github_matches = list_entries(&f.app, &ada.account, "github").await.unwrap();
        assert!(github_matches.len() < builtin.len() / 2);
        let github = builtin.iter().find(|entry| entry.name == "GitHub").unwrap();
        assert_eq!(
            github.template.as_ref().unwrap().url.as_deref(),
            Some("https://api.githubcopilot.com/mcp/")
        );
        assert_eq!(
            resolve(&f.app, &ada.account, &github.id, true)
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::FORBIDDEN
        );
        let created = create_entry(&f.app, &rover.account, input("Rover setup")).unwrap();
        assert!(!ids.contains(&created.id));
        let restarted = App::new((*f.app.config).clone()).unwrap();
        seed(&restarted).unwrap();
        let after = list_entries(&restarted, &ada.account, "").await.unwrap();
        assert_eq!(ids, after.iter().map(|entry| entry.id.clone()).collect());
        assert!(after.iter().all(|entry| entry.version == 1));
        assert_eq!(
            list_entries(&restarted, &rover.account, "")
                .await
                .unwrap()
                .len(),
            builtin.len() + 1
        );
    }
    #[test]
    fn templates_reject_credential_locations_unknown_fields_and_incompatible_transports() {
        for unsafe_url in [
            "https://user:secret@example.com/mcp",
            "https://example.com/mcp?token=secret",
            "https://example.com/#secret",
            "file:///private/key",
            "javascript:alert(1)",
        ] {
            let mut candidate = input("Invalid");
            candidate.source_url = Some(unsafe_url.into());
            assert!(validate(candidate).is_err());
            let mut candidate = input("Invalid");
            candidate.template.as_mut().unwrap().url = Some(unsafe_url.into());
            assert!(validate(candidate).is_err());
        }
        assert!(
            serde_json::from_value::<DirectoryInput>(
                json!({"name":"bad","env":{"TOKEN":"secret"}})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<DirectoryTemplate>(json!({"transport":"http","auth_mode":"shared","headers":{"Authorization":"secret"}})).is_err());
        let mut candidate = input("Invalid");
        candidate.template.as_mut().unwrap().command = Some("sh".into());
        assert!(validate(candidate).is_err());
        for argument in [
            "--token=secret",
            "--api-key",
            "Authorization=secret",
            "Bearer secret",
            "https://host/mcp?token=secret",
        ] {
            let mut candidate = input("Local");
            candidate.template = Some(DirectoryTemplate {
                transport: "stdio".into(),
                url: None,
                command: None,
                args: vec![argument.into()],
                auth_mode: "per-user".into(),
            });
            assert!(validate(candidate).is_err());
        }
        let mut candidate = input("Incomplete");
        candidate.template.as_mut().unwrap().url = None;
        assert!(validate(candidate).is_ok());
        let mut candidate = input("Local");
        candidate.template = Some(DirectoryTemplate {
            transport: "stdio".into(),
            url: None,
            command: Some("npx".into()),
            args: vec!["-y".into(), "example-mcp@1.0.0".into()],
            auth_mode: "none".into(),
        });
        assert!(validate(candidate).is_ok());
    }
    #[tokio::test]
    async fn routes_require_an_accounts_token() {
        let f = people().await;
        for route in [
            "/api/v1/directory",
            "/api/v1/directory/abc",
            "/api/v1/directory/abc/access",
        ] {
            let (status, body) = f.call("GET", route, None, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"]["code"], "authentication_required");
        }
        let (status, _) = f
            .call(
                "POST",
                "/api/v1/directory",
                Some("mpa_old_gateway_token"),
                Some(serde_json::to_value(input("Entry")).unwrap()),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/directory",
                Some(serde_json::to_value(input("Entry")).unwrap()),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}
