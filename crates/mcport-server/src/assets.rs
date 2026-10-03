//! Capability-free result downloads: an index selects bytes already present in
//! an encrypted, caller-owned result. A URL or filesystem path is never accepted.
use crate::{
    auth::{self, Auth},
    connections,
    error::{Error, Result},
    execution::{self, CallRecord},
    state::App,
};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mcport_core::ResultAsset;
use serde_json::{Value, json};

const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
const MAX_ASSETS: usize = 64;
struct EmbeddedAsset {
    name: String,
    mime_type: String,
    source_uri: Option<String>,
    bytes: Vec<u8>,
}

fn clean_mime(value: Option<&str>) -> String {
    let mime = value
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if mime.len() <= 100
        && mime.contains('/')
        && mime
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/!#$&^_.+-".contains(&c))
    {
        mime.to_ascii_lowercase()
    } else {
        "application/octet-stream".into()
    }
}
fn extension(mime: &str) -> &'static str {
    match mime {
        "text/plain" => "txt",
        "application/json" => "json",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}
fn filename(uri: Option<&str>, mime: &str, index: usize) -> String {
    let name = uri.and_then(|s| url::Url::parse(s).ok()).and_then(|u| {
        u.path_segments()
            .and_then(|mut p| p.next_back())
            .map(str::to_owned)
    });
    let cleaned = name
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(100)
        .collect::<String>();
    if !cleaned.is_empty() && !matches!(cleaned.as_str(), "." | "..") && !cleaned.starts_with('.') {
        cleaned
    } else {
        format!("result-{}.{}", index + 1, extension(mime))
    }
}
fn push_resource(assets: &mut Vec<EmbeddedAsset>, resource: &Value) {
    if assets.len() >= MAX_ASSETS {
        return;
    }
    let uri = resource
        .get("uri")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mime = clean_mime(resource.get("mimeType").and_then(Value::as_str));
    let bytes = if let Some(text) = resource.get("text").and_then(Value::as_str) {
        if text.len() > MAX_ASSET_BYTES {
            return;
        }
        text.as_bytes().to_vec()
    } else if let Some(blob) = resource
        .get("blob")
        .or_else(|| resource.get("data"))
        .and_then(Value::as_str)
    {
        if blob.len() > MAX_ASSET_BYTES.div_ceil(3) * 4 {
            return;
        }
        let Ok(data) = STANDARD.decode(blob) else {
            return;
        };
        if data.len() > MAX_ASSET_BYTES {
            return;
        }
        data
    } else {
        return;
    };
    let mime = if resource.get("text").is_some() && resource.get("mimeType").is_none() {
        "text/plain".into()
    } else {
        mime
    };
    assets.push(EmbeddedAsset {
        name: filename(uri.as_deref(), &mime, assets.len()),
        mime_type: mime,
        source_uri: uri,
        bytes,
    });
}
fn embedded(result: &Value) -> Vec<EmbeddedAsset> {
    let mut assets = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content.iter().take(MAX_ASSETS) {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => push_resource(&mut assets, block),
                Some("image" | "audio") => push_resource(&mut assets, block),
                Some("resource") => {
                    if let Some(resource) = block.get("resource") {
                        push_resource(&mut assets, resource)
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(contents) = result.get("contents").and_then(Value::as_array) {
        for resource in contents.iter().take(MAX_ASSETS) {
            push_resource(&mut assets, resource)
        }
    }
    if let Some(materialized) = result
        .pointer("/_meta/mcport/assets")
        .and_then(Value::as_array)
    {
        for resource in materialized.iter().take(MAX_ASSETS) {
            push_resource(&mut assets, resource)
        }
    }
    if let Some(structured) = result.get("structuredContent")
        && assets.len() < MAX_ASSETS
        && let Ok(bytes) = serde_json::to_vec_pretty(structured)
        && bytes.len() <= MAX_ASSET_BYTES
    {
        assets.push(EmbeddedAsset {
            name: "structured-output.json".into(),
            mime_type: "application/json".into(),
            source_uri: None,
            bytes,
        });
    }
    assets
}
/// Uses today's IAM authorization and tool policies, not a saved session snapshot.
/// A fresh session for the same actor can download; revoked access cannot.
fn authorized(app: &App, actor: &Auth, id: &str) -> Result<CallRecord> {
    let record = execution::owned(app, actor, id)?;
    app.assert_generation(&record.environment, record.generation)?;
    let connection = connections::resolve(app, actor, &record.invocation.connection_id, false)?;
    if let Some(tool) = &record.invocation.tool_name
        && !connections::allowed_tool(app, &connection, actor, tool)?
    {
        return Err(Error::denied());
    }
    if record.invocation.result.is_none() {
        return Err(Error::new(
            404,
            "result_unavailable",
            "This invocation does not have a completed result to download.",
            "Inspect activity and wait for completion; do not repeat a potentially completed action.",
        ));
    }
    Ok(record)
}

pub async fn list(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let actor = auth::authenticate(&app, &headers).await?;
    let record = authorized(&app, &actor, &id)?;
    let assets = embedded(record.invocation.result.as_ref().expect("checked result"));
    let items: Vec<ResultAsset> = assets
        .into_iter()
        .enumerate()
        .map(|(index, asset)| ResultAsset {
            index: index as u32,
            name: asset.name,
            mime_type: asset.mime_type,
            size: asset.bytes.len() as u64,
            source_uri: asset.source_uri,
            download_url: format!("/api/v1/calls/{}/assets/{index}", record.invocation.id),
        })
        .collect();
    Ok(Json(json!({"data":items})))
}
fn response_content_type(mime: &str) -> &str {
    match mime {
        "text/plain" | "application/json" | "image/png" | "image/jpeg" | "image/gif"
        | "image/webp" | "audio/mpeg" | "audio/wav" | "audio/ogg" | "audio/flac" | "audio/mp4"
        | "application/pdf" => mime,
        _ => "application/octet-stream",
    }
}
fn attachment(asset: EmbeddedAsset) -> Result<Response> {
    let mut response = Response::new(Body::from(asset.bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(response_content_type(&asset.mime_type))
            .map_err(|_| Error::internal())?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", asset.name))
            .map_err(|_| Error::internal())?,
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    Ok(response)
}
pub async fn download(
    State(app): State<App>,
    headers: HeaderMap,
    Path((id, index)): Path<(String, u32)>,
) -> Result<Response> {
    let actor = auth::authenticate(&app, &headers).await?;
    let record = authorized(&app, &actor, &id)?;
    let asset = embedded(record.invocation.result.as_ref().expect("checked result"))
        .into_iter()
        .nth(index as usize)
        .ok_or_else(Error::missing)?;
    attachment(asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_assets_are_only_embedded_bytes() {
        let result = json!({"content":[{"type":"text","text":"plain result"},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"},{"type":"resource_link","uri":"file:///etc/passwd"},{"type":"resource","resource":{"uri":"notes://example","text":"private note"}}],"_meta":{"mcport":{"assets":[{"uri":"http://127.0.0.1:3845/assets/icon.svg","mimeType":"image/svg+xml","blob":"PHN2Zz48L3N2Zz4="}]}},"structuredContent":{"ok":true}});
        let assets = embedded(&result);
        assert_eq!(assets.len(), 5);
        assert_eq!(assets[0].bytes, b"plain result");
        assert_eq!(assets[1].bytes, b"hello");
        assert_eq!(assets[3].name, "icon.svg");
        assert_eq!(assets[4].name, "structured-output.json");
    }
    #[test]
    fn malformed_blobs_do_not_turn_into_asset_capabilities() {
        let assets = embedded(
            &json!({"content":[{"type":"image","data":"not base64","mimeType":"image/png"},{"type":"resource_link","uri":"http://localhost/assets/file"}]}),
        );
        assert!(assets.is_empty());
    }
    #[test]
    fn active_content_is_forced_to_attachment_and_nosniff() {
        let response = attachment(EmbeddedAsset {
            name: "asset.svg".into(),
            mime_type: "image/svg+xml".into(),
            source_uri: None,
            bytes: b"<svg/>".to_vec(),
        })
        .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(
            response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; sandbox"
        );
    }
    #[test]
    fn untrusted_headers_and_filenames_are_normalized() {
        assert_eq!(
            clean_mime(Some("text/html\r\nX-Injected: yes")),
            "application/octet-stream"
        );
        let name = filename(
            Some("file:///tmp/../../secret%22%0D%0A.txt"),
            "text/plain",
            0,
        );
        assert!(!name.contains(['\r', '\n', '"', '/']));
    }
    #[test]
    fn downloads_recheck_caller_org_environment_grants_and_tool_policy() {
        use crate::{
            auth::StoredSession,
            connections::{Grant, Policy},
            state::{Config, now},
        };
        use mcport_core::{AccessGrant, Actor, Connection, Invocation, ToolPolicy};
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::from_env();
        config.data_dir = directory.path().into();
        let app = App::new(config).unwrap();
        let actor = Auth {
            session: StoredSession {
                key: "new-session".into(),
                family: "new-family".into(),
                actor: Actor {
                    principal_id: "si:runner".into(),
                    identity_kind: "silicon".into(),
                    org_id: "org".into(),
                    display_name: "Runner".into(),
                },
                environment: "production".into(),
                generation: 0,
                control_revision: 0,
                iam_access: "fixture".into(),
                iam_refresh: "fixture".into(),
                iam_expires: now() + 60,
                expires_at: now() + 60,
                refresh_key: None,
            },
        };
        let connection = Connection {
            id: "conn".into(),
            name: "assets".into(),
            description: "".into(),
            org_id: "org".into(),
            owner_id: "c:owner".into(),
            environment: "production".into(),
            transport: "http".into(),
            url: Some("https://example.com/mcp".into()),
            host_id: None,
            command: None,
            args: vec![],
            auth_mode: "none".into(),
            visibility: "invited".into(),
            status: "ready".into(),
            can_manage: false,
            account: None,
            created_at: now(),
            updated_at: now(),
            version: 1,
        };
        app.store
            .put(
                "connection",
                "conn",
                "production",
                "org",
                "c:owner",
                Some("assets"),
                &connection,
                None,
            )
            .unwrap();
        let grant = Grant {
            connection_id: "conn".into(),
            grant: AccessGrant {
                principal_id: "si:runner".into(),
                created_at: now(),
            },
        };
        let grant_key = connections::grant_key("conn", "si:runner");
        app.store
            .put(
                "grant",
                &grant_key,
                "production",
                "org",
                "c:owner",
                None,
                &grant,
                None,
            )
            .unwrap();
        let record = CallRecord {
            invocation: Invocation {
                id: "call".into(),
                connection_id: "conn".into(),
                connection_name: "assets".into(),
                actor_id: "si:runner".into(),
                execution_account_id: "si:runner".into(),
                method: "tools/call".into(),
                tool_name: Some("design".into()),
                status: "completed".into(),
                created_at: now(),
                completed_at: Some(now()),
                result: Some(json!({"content":[{"type":"text","text":"private asset"}]})),
                error: None,
            },
            environment: "production".into(),
            org_id: "org".into(),
            family: "old-family".into(),
            generation: 0,
            host_id: None,
            params: json!({}),
            timeout_ms: 1000,
            expires_at: now() + 1,
            connection_version: 1,
            fingerprint: "fixture".into(),
            progress: None,
            telemetry_enabled: false,
        };
        execution::save(&app, &record).unwrap();
        assert!(
            authorized(&app, &actor, "call").is_ok(),
            "fresh session for same current actor remains usable"
        );
        let mut stranger = actor.clone();
        stranger.session.actor.principal_id = "si:stranger".into();
        assert!(authorized(&app, &stranger, "call").is_err());
        let mut other_org = actor.clone();
        other_org.session.actor.org_id = "other".into();
        assert!(authorized(&app, &other_org, "call").is_err());
        let mut other_env = actor.clone();
        other_env.session.environment = "other".into();
        assert!(authorized(&app, &other_env, "call").is_err());
        app.store.delete("grant", &grant_key).unwrap();
        assert!(
            authorized(&app, &actor, "call").is_err(),
            "revoked invitation blocks old result downloads"
        );
        app.store
            .put(
                "grant",
                &grant_key,
                "production",
                "org",
                "c:owner",
                None,
                &grant,
                None,
            )
            .unwrap();
        let policy = Policy {
            connection_id: "conn".into(),
            policy: ToolPolicy {
                tool: "design".into(),
                principal_id: None,
                enabled: false,
            },
        };
        app.store
            .put(
                "policy",
                &connections::policy_key("conn", "design", None),
                "production",
                "org",
                "c:owner",
                None,
                &policy,
                None,
            )
            .unwrap();
        assert!(
            authorized(&app, &actor, "call").is_err(),
            "current tool policy guards old results"
        );
    }
}
