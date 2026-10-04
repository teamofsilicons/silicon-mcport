//! Discovery metadata only: no network requests, credentials, or executable actions.
use crate::{
    auth::{self, Auth},
    error::{Error, Result},
    state::{App, hash, now},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use mcport_core::{DirectoryEntry, DirectoryInput, DirectoryTemplate, DirectoryUpdate};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

const CATALOG_ENV: &str = "catalog";
const MAX_ORG_ENTRIES: usize = 1000;

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

fn input_of(entry: &DirectoryEntry) -> DirectoryInput {
    DirectoryInput {
        name: entry.name.clone(),
        description: entry.description.clone(),
        category: entry.category.clone(),
        source_url: entry.source_url.clone(),
        template: entry.template.clone(),
    }
}
fn assign(entry: &mut DirectoryEntry, input: DirectoryInput) {
    entry.name = input.name;
    entry.description = input.description;
    entry.category = input.category;
    entry.source_url = input.source_url;
    entry.template = input.template;
}
fn visible(entry: &DirectoryEntry, auth: &Auth) -> bool {
    (entry.source == "community" && entry.environment == CATALOG_ENV && entry.org_id.is_empty())
        || (entry.source == "org"
            && entry.environment == auth.env()
            && entry.org_id == auth.actor().org_id)
}
fn view(mut entry: DirectoryEntry, auth: &Auth) -> DirectoryEntry {
    entry.can_manage = entry.source == "org"
        && visible(&entry, auth)
        && entry.owner_id == auth.actor().principal_id;
    entry
}
fn resolve(app: &App, auth: &Auth, id: &str, manage: bool) -> Result<DirectoryEntry> {
    let entry = app
        .store
        .get::<DirectoryEntry>("directory", id)?
        .or(app.store.get::<DirectoryEntry>("catalog", id)?)
        .ok_or_else(Error::missing)?;
    if !visible(&entry, auth) {
        return Err(Error::missing());
    }
    let entry = view(entry, auth);
    if manage && !entry.can_manage {
        return Err(Error::new(
            403,
            "access_denied",
            "Only the creator can change an organization directory entry; community entries are read-only.",
            "Add your own organization entry to use different setup defaults.",
        ));
    }
    Ok(entry)
}
fn list_entries(app: &App, auth: &Auth, query: &str) -> Result<Vec<DirectoryEntry>> {
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err(Error::bad(
            "Directory search must be at most 256 bytes without control characters.",
        ));
    }
    let query = query.trim().to_lowercase();
    let mut entries = app
        .store
        .list::<DirectoryEntry>("catalog", Some(CATALOG_ENV))?;
    entries.extend(
        app.store
            .list::<DirectoryEntry>("directory", Some(auth.env()))?,
    );
    entries.retain(|entry| {
        visible(entry, auth)
            && (query.is_empty()
                || [
                    entry.name.as_str(),
                    entry.description.as_str(),
                    entry.category.as_str(),
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&query)))
    });
    entries.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.id.clone()));
    Ok(entries.into_iter().map(|entry| view(entry, auth)).collect())
}
fn create_entry(app: &App, auth: &Auth, input: DirectoryInput) -> Result<DirectoryEntry> {
    let input = validate(input)?;
    if app
        .store
        .list::<DirectoryEntry>("directory", Some(auth.env()))?
        .iter()
        .filter(|entry| entry.org_id == auth.actor().org_id)
        .count()
        >= MAX_ORG_ENTRIES
    {
        return Err(Error::bad(
            "This organization has reached the 1000-entry directory limit.",
        ));
    }
    let mut entry = DirectoryEntry {
        id: String::new(),
        name: input.name.clone(),
        description: input.description,
        category: input.category,
        source: "org".into(),
        source_url: input.source_url,
        source_revision: None,
        owner_id: auth.actor().principal_id.clone(),
        org_id: auth.actor().org_id.clone(),
        environment: auth.env().into(),
        can_manage: false,
        template: input.template,
        version: 1,
        created_at: now(),
        updated_at: now(),
    };
    let name = entry.name.clone();
    let (entry, _) = app.store.create_public(
        "directory",
        auth.env(),
        &auth.actor().org_id,
        &auth.actor().principal_id,
        Some(&name),
        None,
        |id| {
            entry.id = id;
            entry
        },
    )?;
    Ok(view(entry, auth))
}
fn update_entry(
    app: &App,
    auth: &Auth,
    id: &str,
    update: DirectoryUpdate,
) -> Result<DirectoryEntry> {
    let input = validate(update.input)?;
    let mut entry = resolve(app, auth, id, true)?;
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
    entry.can_manage = false;
    app.store.put(
        "directory",
        &entry.id,
        &entry.environment,
        &entry.org_id,
        &entry.owner_id,
        Some(&entry.name),
        &entry,
        Some(update.version),
    )?;
    Ok(view(entry, auth))
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
            |id| DirectoryEntry {
                id,
                name: input.name.clone(),
                description: input.description.clone(),
                category: input.category.clone(),
                source: "community".into(),
                source_url: input.source_url.clone(),
                source_revision: Some(snapshot.revision.clone()),
                owner_id: String::new(),
                org_id: String::new(),
                environment: CATALOG_ENV.into(),
                can_manage: false,
                template: input.template.clone(),
                version: 1,
                created_at: timestamp,
                updated_at: timestamp,
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
        .list::<DirectoryEntry>("catalog", Some(CATALOG_ENV))?
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
    headers: HeaderMap,
    Query(search): Query<Search>,
) -> Result<Json<Value>> {
    let auth = auth::authenticate(&app, &headers).await?;
    Ok(Json(json!({"data": list_entries(&app, &auth, &search.q)?})))
}
pub async fn get(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let auth = auth::authenticate(&app, &headers).await?;
    Ok(Json(json!({"data": resolve(&app, &auth, &id, false)?})))
}
pub async fn create(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<DirectoryInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let auth = auth::authenticate(&app, &headers).await?;
    let _guard = auth::mutation_guard(&app, &auth).await?;
    Ok(Json(json!({"data": create_entry(&app, &auth, input)?})))
}
pub async fn update(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<DirectoryUpdate>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let auth = auth::authenticate(&app, &headers).await?;
    let _guard = auth::mutation_guard(&app, &auth).await?;
    Ok(Json(
        json!({"data": update_entry(&app, &auth, &id, input)?}),
    ))
}
pub async fn remove(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let auth = auth::authenticate(&app, &headers).await?;
    let _guard = auth::mutation_guard(&app, &auth).await?;
    let entry = resolve(&app, &auth, &id, true)?;
    app.store.delete("directory", &entry.id)?;
    Ok(Json(json!({"data":{"deleted":true}})))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (App, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let mut config = crate::state::Config::from_env();
        config.data_dir = directory.path().into();
        config.test_app_secrets = "{}".into();
        (App::new(config).unwrap(), directory)
    }
    fn actor(owner: &str, org: &str, environment: &str) -> Auth {
        Auth { session: serde_json::from_value(json!({"key":"fixture-session","family":"fixture-family", "actor":{"principal_id":owner,"identity_kind":if owner.starts_with("si:") {"silicon"} else {"carbon"},"org_id":org,"display_name":"Fixture"},"environment":environment,"generation":0,"control_revision":0,"iam_access":"fixture","iam_refresh":"fixture","iam_expires":0,"expires_at":0,"refresh_key":null})).unwrap() }
    }
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
    #[test]
    fn organization_members_can_discover_but_only_creator_can_edit_across_tenant_boundaries() {
        let (app, _directory) = fixture();
        let owner = actor("c:owner", "tos", "production");
        let silicon = actor("si:colleague", "tos", "production");
        let foreign = actor("c:owner", "another-org", "production");
        let test = actor("c:owner", "tos", "test-isolated");
        let entry = create_entry(&app, &owner, input("Payments Directory")).unwrap();
        assert_eq!(entry.id.len(), 3);
        assert_eq!(entry.category, "Other");
        assert!(entry.can_manage);
        assert!(
            !resolve(&app, &silicon, &entry.id, false)
                .unwrap()
                .can_manage
        );
        assert_eq!(list_entries(&app, &silicon, "PAYMENTS").unwrap().len(), 1);
        for other in [&foreign, &test] {
            assert!(list_entries(&app, other, "").unwrap().is_empty());
            assert_eq!(
                resolve(&app, other, &entry.id, false).unwrap_err().0,
                axum::http::StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            resolve(&app, &silicon, &entry.id, true).unwrap_err().0,
            axum::http::StatusCode::FORBIDDEN
        );
        let own = create_entry(&app, &silicon, input("Colleague entry")).unwrap();
        assert!(own.can_manage);
        let updated = update_entry(
            &app,
            &owner,
            &entry.id,
            DirectoryUpdate {
                input: input("Renamed setup"),
                version: 1,
            },
        )
        .unwrap();
        assert_eq!(updated.version, 2);
        let stale = update_entry(
            &app,
            &owner,
            &entry.id,
            DirectoryUpdate {
                input: input("Stale write"),
                version: 1,
            },
        )
        .unwrap_err();
        assert_eq!(stale.1.code, "revision_conflict");
        assert_eq!(
            resolve(&app, &owner, &entry.id, false).unwrap().name,
            "Renamed setup"
        );
        assert!(
            app.store
                .list::<mcport_core::Connection>("connection", None)
                .unwrap()
                .is_empty()
        );
        assert!(
            app.store
                .list::<Value>("credential", None)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn bundled_catalog_is_licensed_stable_read_only_and_survives_environment_cleanup() {
        let (app, _directory) = fixture();
        seed(&app).unwrap();
        let auth = actor("c:owner", "tos", "production");
        let builtin = list_entries(&app, &auth, "").unwrap();
        assert!(builtin.len() >= 500);
        let ids: HashSet<_> = builtin.iter().map(|entry| entry.id.clone()).collect();
        assert_eq!(ids.len(), builtin.len());
        assert!(builtin.iter().all(|entry| {
            entry.id.len() == 3
                && !entry.can_manage
                && entry
                    .source_revision
                    .as_ref()
                    .is_some_and(|revision| revision.len() == 40)
                && entry.source_url.is_some()
        }));
        let github_matches = list_entries(&app, &auth, "github").unwrap();
        assert!(github_matches.len() < builtin.len() / 2);
        assert!(github_matches.iter().all(|entry| {
            format!("{} {} {}", entry.name, entry.description, entry.category)
                .to_lowercase()
                .contains("github")
        }));
        let github = builtin.iter().find(|entry| entry.name == "GitHub").unwrap();
        assert_eq!(
            github.template.as_ref().unwrap().url.as_deref(),
            Some("https://api.githubcopilot.com/mcp/")
        );
        assert_eq!(
            resolve(&app, &auth, &github.id, true).unwrap_err().0,
            axum::http::StatusCode::FORBIDDEN
        );
        let test = actor("si:tester", "tos", "test-world");
        let created = create_entry(&app, &test, input("Test-only setup")).unwrap();
        assert!(!ids.contains(&created.id));
        app.store.clear_environment("test-world").unwrap();
        assert!(resolve(&app, &test, &created.id, false).is_err());
        let restarted = App::new((*app.config).clone()).unwrap();
        seed(&restarted).unwrap();
        let after = list_entries(&restarted, &auth, "").unwrap();
        assert_eq!(ids, after.iter().map(|entry| entry.id.clone()).collect());
        assert!(after.iter().all(|entry| entry.version == 1));
        assert_eq!(
            list_entries(&restarted, &test, "").unwrap().len(),
            builtin.len()
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
    async fn routes_require_session_and_reject_cross_origin_writes() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let (app, _directory) = fixture();
        let router = crate::router(app);
        for route in ["/api/v1/directory", "/api/v1/directory/abc"] {
            let response = router
                .clone()
                .oneshot(Request::get(route).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = router
            .oneshot(
                Request::post("/api/v1/directory")
                    .header("Content-Type", "application/json")
                    .header("Origin", "https://evil.example")
                    .header("Cookie", "mcport_session=fixture")
                    .body(Body::from(serde_json::to_vec(&input("Entry")).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
