//! Result downloads: an index selects bytes already present in an encrypted
//! result the caller (or its custodian) may read. A URL or filesystem path is
//! never accepted. One-time tickets let a browser download without a token.
use crate::{
    accounts::AccountRow,
    auth::Auth,
    error::{Error, Result},
    execution::{self, CallRecord},
    state::{App, hash, now, secret},
};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mcport_core::{DownloadTicket, ResultAsset};
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
/// Current authorization, not a snapshot: the viewer is the caller or the
/// caller's custodian, and the caller must still be able to use the connection
/// and tool. Revoked access hides old results.
async fn authorized(app: &App, viewer: &AccountRow, id: &str) -> Result<CallRecord> {
    let record = execution::owned(app, viewer, id).await?;
    execution::readable(app, &record).await?;
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

/// A one-time download link for one asset; redeeming it re-checks authorization.
#[derive(Clone)]
pub struct Ticket {
    call_id: String,
    index: u32,
    viewer_uuid: String,
    expires_at: i64,
}
const TICKET_SECONDS: i64 = 60;

pub async fn list(State(app): State<App>, a: Auth, Path(id): Path<String>) -> Result<Json<Value>> {
    let record = authorized(&app, &a.account, &id).await?;
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
    a: Auth,
    Path((id, index)): Path<(String, u32)>,
) -> Result<Response> {
    let record = authorized(&app, &a.account, &id).await?;
    let asset = embedded(record.invocation.result.as_ref().expect("checked result"))
        .into_iter()
        .nth(index as usize)
        .ok_or_else(Error::missing)?;
    attachment(asset)
}
/// `POST /api/v1/calls/{call}/assets/{index}/ticket`: a link that downloads this
/// asset once within 60 seconds without a token (for browsers behind the website).
pub async fn ticket(
    State(app): State<App>,
    a: Auth,
    Path((id, index)): Path<(String, u32)>,
) -> Result<Json<Value>> {
    let record = authorized(&app, &a.account, &id).await?;
    if embedded(record.invocation.result.as_ref().expect("checked result")).len() <= index as usize
    {
        return Err(Error::missing());
    }
    let value = secret("mpd_");
    let expires_at = now() + TICKET_SECONDS;
    {
        let mut tickets = app.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, ticket| ticket.expires_at > now());
        if tickets.len() >= 10_000 {
            return Err(Error::new(
                503,
                "too_many_downloads",
                "Too many download links are waiting to be used.",
                "Retry in a minute.",
            ));
        }
        tickets.insert(
            hash(&value),
            Ticket {
                call_id: record.invocation.id.clone(),
                index,
                viewer_uuid: a.uuid().into(),
                expires_at,
            },
        );
    }
    Ok(Json(json!({"data":DownloadTicket{
        url: format!("{}/api/v1/downloads/{value}", app.config.public_url.trim_end_matches('/')),
        expires_at,
    }})))
}
/// `GET /api/v1/downloads/{ticket}`: redeem a ticket (once).
pub async fn redeem(State(app): State<App>, Path(value): Path<String>) -> Result<Response> {
    let ticket = app
        .tickets
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&hash(&value))
        .filter(|ticket| ticket.expires_at > now())
        .ok_or_else(|| {
            Error::new(
                404,
                "download_expired",
                "This download link was already used or has expired.",
                "Ask for a new download link.",
            )
        })?;
    let viewer = app
        .store
        .account(&ticket.viewer_uuid)?
        .filter(AccountRow::active)
        .ok_or_else(Error::missing)?;
    let record = authorized(&app, &viewer, &ticket.call_id).await?;
    let asset = embedded(record.invocation.result.as_ref().expect("checked result"))
        .into_iter()
        .nth(ticket.index as usize)
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
    /// A completed call by `caller` on `connection`, stored directly.
    fn completed(app: &App, id: &str, caller: &str, connection: &str) {
        let record = CallRecord {
            invocation: crate::execution::InvocationData {
                id: id.into(),
                connection_id: connection.into(),
                connection_name: "assets".into(),
                method: "tools/call".into(),
                tool_name: Some("design".into()),
                status: "completed".into(),
                created_at: crate::state::now(),
                completed_at: Some(crate::state::now()),
                result: Some(json!({"content":[{"type":"text","text":"private asset"}]})),
                ..Default::default()
            },
            caller_uuid: caller.into(),
            params: json!({}),
            timeout_ms: 1000,
            expires_at: crate::state::now() + 1,
            connection_version: 1,
            fingerprint: id.into(),
            ..Default::default()
        };
        execution::save(app, &record).unwrap();
    }
    #[tokio::test]
    async fn downloads_recheck_the_callers_access_grants_and_tool_policy() {
        use crate::test_support::{connection, fixture};
        use axum::http::StatusCode;
        let f = fixture().await;
        f.carbon("Owner", "c:owner");
        f.carbon("Ada", "c:ada");
        f.silicon("Runner", "si:runner", "Ada");
        f.carbon("Stranger", "c:stranger");
        let (_, body) = f
            .as_(
                "Owner",
                "POST",
                "/api/v1/connections",
                Some(connection("assets", "none", "")),
            )
            .await;
        let cid = body["data"]["id"].as_str().unwrap().to_owned();
        f.as_(
            "Ada",
            "POST",
            "/api/v1/allow",
            Some(json!({"account":"c:owner","silicon":"si:runner"})),
        )
        .await;
        let (status, body) = f
            .as_(
                "Owner",
                "POST",
                &format!("/api/v1/connections/{cid}/access"),
                Some(json!({"account":"si:runner"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        f.auth("Runner").await;
        completed(&f.app, "cAl", "Runner", &cid);
        let assets = "/api/v1/calls/cAl/assets";
        // The caller and the caller's custodian may read; the connection owner
        // and strangers may not (results belong to the caller).
        for (who, expected) in [
            ("Runner", StatusCode::OK),
            ("Ada", StatusCode::OK),
            ("Owner", StatusCode::NOT_FOUND),
            ("Stranger", StatusCode::NOT_FOUND),
        ] {
            assert_eq!(f.as_(who, "GET", assets, None).await.0, expected, "{who}");
        }
        let (status, body) = f.as_("Ada", "GET", "/api/v1/calls/cAl", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["caller"]["id"], "si:runner");
        let (_, body) = f.as_("Ada", "GET", "/api/v1/calls", None).await;
        assert_eq!(
            body["data"].as_array().unwrap().len(),
            1,
            "custodians see their Silicons' activity"
        );
        assert_eq!(
            f.as_("Owner", "GET", "/api/v1/calls", None).await.1["data"],
            json!([])
        );
        // Removing the caller's access hides old results from everyone.
        f.as_(
            "Owner",
            "DELETE",
            &format!("/api/v1/connections/{cid}/access/si:runner"),
            None,
        )
        .await;
        for who in ["Runner", "Ada"] {
            assert_eq!(
                f.as_(who, "GET", assets, None).await.0,
                StatusCode::NOT_FOUND,
                "{who}"
            );
        }
        f.as_(
            "Owner",
            "POST",
            &format!("/api/v1/connections/{cid}/access"),
            Some(json!({"account":"si:runner"})),
        )
        .await;
        assert_eq!(f.as_("Runner", "GET", assets, None).await.0, StatusCode::OK);
        // So does disabling the tool for the caller.
        f.as_(
            "Owner",
            "PUT",
            &format!("/api/v1/connections/{cid}/policies"),
            Some(json!({"tool":"design","account":"si:runner","enabled":false})),
        )
        .await;
        assert_eq!(
            f.as_("Runner", "GET", assets, None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn download_tickets_work_once_within_a_minute_and_recheck_access() {
        use crate::test_support::{connection, fixture};
        use axum::http::StatusCode;
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.carbon("Cy", "c:cy");
        let (_, body) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/connections",
                Some(connection("assets", "none", "")),
            )
            .await;
        let cid = body["data"]["id"].as_str().unwrap().to_owned();
        completed(&f.app, "cAl", "Ada", &cid);
        let (status, body) = f
            .as_("Cy", "POST", "/api/v1/calls/cAl/assets/0/ticket", None)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = f
            .as_("Ada", "POST", "/api/v1/calls/cAl/assets/9/ticket", None)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = f
            .as_("Ada", "POST", "/api/v1/calls/cAl/assets/0/ticket", None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let url = body["data"]["url"].as_str().unwrap();
        assert!(
            url.starts_with("http://127.0.0.1:4241/api/v1/downloads/mpd_"),
            "{url}"
        );
        assert!(body["data"]["expires_at"].as_i64().unwrap() <= crate::state::now() + 60);
        let path = url.trim_start_matches("http://127.0.0.1:4241");
        use tower::ServiceExt;
        let response = crate::router(f.app.clone())
            .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain");
        assert!(
            response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"private asset");
        let (status, body) = f.call("GET", path, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "download_expired");
        // A ticket issued before access ended does not outlive it.
        let (_, body) = f
            .as_("Ada", "POST", "/api/v1/calls/cAl/assets/0/ticket", None)
            .await;
        let path = body["data"]["url"]
            .as_str()
            .unwrap()
            .trim_start_matches("http://127.0.0.1:4241")
            .to_owned();
        f.as_("Ada", "DELETE", &format!("/api/v1/connections/{cid}"), None)
            .await;
        assert_eq!(
            f.call("GET", &path, None, None).await.0,
            StatusCode::NOT_FOUND
        );
    }
}
