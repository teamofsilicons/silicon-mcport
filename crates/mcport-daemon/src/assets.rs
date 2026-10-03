//! Copies small, ephemeral assets explicitly returned by the current MCP call.
//! This is not a generic download proxy and never opens host filesystem paths.
use base64::{Engine, engine::general_purpose::STANDARD};
use mcport_mcp::Endpoint;
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};
use tokio_util::sync::CancellationToken;
use url::Url;

const MAX_COUNT: usize = 16;
const MAX_FILE: usize = 5 * 1024 * 1024;
const MAX_TOTAL: usize = 10 * 1024 * 1024;
const MAX_RESULT: usize = 16 * 1024 * 1024;

/// Inspect the literal path before URL parsing, which otherwise removes dot
/// segments. Percent escapes and query/fragment are intentionally unsupported:
/// there is no second decode or routing ambiguity on the asset origin.
fn asset_url(raw: &str, base: &Url) -> Option<Url> {
    if raw.len() > 4096
        || raw.contains(['%', '\\', '?', '#'])
        || raw.chars().any(char::is_whitespace)
    {
        return None;
    }
    let authority = raw.split_once("://")?.1;
    let path = &authority[authority.find('/')?..];
    if !path.starts_with("/assets/") {
        return None;
    }
    for segment in path[8..].split('/') {
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || !segment
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.~".contains(&c))
        {
            return None;
        }
    }
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url)
}
fn candidates(value: &Value, base: &Url, out: &mut BTreeSet<String>, depth: usize) {
    if depth > 32 || out.len() >= MAX_COUNT {
        return;
    }
    match value {
        Value::String(text) => {
            if let Some(url) = asset_url(text, base) {
                out.insert(url.to_string());
                return;
            }
            // Figma's design-context response includes exact /assets URLs in
            // returned text/code. Only literal URL tokens may become candidates.
            for (index, _) in text.match_indices("http") {
                if out.len() >= MAX_COUNT {
                    break;
                }
                let tail = &text[index..];
                if !tail.starts_with("http://") && !tail.starts_with("https://") {
                    continue;
                }
                let token = tail
                    .split(|c: char| {
                        c.is_whitespace()
                            || matches!(
                                c,
                                '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '{' | '}' | '[' | ']'
                            )
                    })
                    .next()
                    .unwrap_or("");
                // Sentence punctuation is outside a literal text URL; quoted
                // resource URIs above retain their exact filename.
                let token = token.trim_end_matches([',', ';', '.']);
                if let Some(url) = asset_url(token, base) {
                    out.insert(url.to_string());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                candidates(item, base, out, depth + 1);
                if out.len() >= MAX_COUNT {
                    break;
                }
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if matches!(key.as_str(), "blob" | "data") {
                    continue;
                }
                candidates(item, base, out, depth + 1);
                if out.len() >= MAX_COUNT {
                    break;
                }
            }
        }
        _ => {}
    }
}
fn content_type(response: &reqwest::Response) -> String {
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next())
        .unwrap_or("application/octet-stream")
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
/// Materialization failure never replaces a completed tool's outcome with an
/// error, and never retries the tool. Original content is preserved.
pub async fn materialize(
    endpoint: &Endpoint,
    result: &mut Value,
    cancellation: &CancellationToken,
    budget: Duration,
) {
    if cancellation.is_cancelled() || budget.is_zero() {
        return;
    }
    let Endpoint::Http {
        url,
        headers,
        bearer_token,
    } = endpoint
    else {
        return;
    };
    let Ok(base) = Url::parse(url) else { return };
    if !result.is_object() {
        return;
    }
    if result.get("_meta").is_some_and(|v| !v.is_object())
        || result
            .pointer("/_meta/mcport")
            .is_some_and(|v| !v.is_object())
        || result
            .pointer("/_meta/mcport/assets")
            .is_some_and(|v| !v.is_array())
    {
        return;
    }
    let mut urls = BTreeSet::new();
    for key in ["content", "contents", "structuredContent"] {
        if let Some(value) = result.get(key) {
            candidates(value, &base, &mut urls, 0)
        }
    }
    if urls.is_empty() {
        return;
    }
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(700))
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return;
    };
    let original_size = serde_json::to_vec(result)
        .map(|v| v.len())
        .unwrap_or(MAX_RESULT);
    if original_size >= MAX_RESULT {
        return;
    }
    let deadline = tokio::time::Instant::now() + budget.min(Duration::from_secs(3));
    let mut total = 0usize;
    let mut encoded_total = 0usize;
    let mut resources = Vec::new();
    let mut unavailable = 0usize;
    for raw in urls {
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            break;
        }
        let Some(asset) = asset_url(&raw, &base) else {
            continue;
        };
        let mut request = client.get(asset.clone());
        // Endpoint already contains the selected caller/shared account. These
        // values can only reach the identical scheme, host and port.
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
                continue;
            }
            request = request.header(name, value)
        }
        if let Some(token) = bearer_token {
            request = request.bearer_auth(token)
        }
        let fetch = async {
            let mut response = request.send().await.ok()?;
            if !response.status().is_success()
                || response
                    .content_length()
                    .is_some_and(|n| n > MAX_FILE as u64)
            {
                return None;
            }
            let mime = content_type(&response);
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.ok()? {
                if bytes.len() + chunk.len() > MAX_FILE
                    || total + bytes.len() + chunk.len() > MAX_TOTAL
                {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            Some((mime, bytes))
        };
        let fetched = tokio::select! {_=cancellation.cancelled()=>break,output=tokio::time::timeout_at(deadline,fetch)=>output.ok().flatten()};
        let Some((mime, bytes)) = fetched else {
            unavailable += 1;
            continue;
        };
        let resource = json!({"uri":asset.as_str(),"mimeType":mime,"blob":STANDARD.encode(&bytes)});
        let size = serde_json::to_vec(&resource)
            .map(|v| v.len())
            .unwrap_or(MAX_RESULT);
        if original_size + encoded_total + size + 1024 > MAX_RESULT {
            unavailable += 1;
            break;
        }
        total += bytes.len();
        encoded_total += size;
        resources.push(resource);
    }
    if !resources.is_empty() || unavailable > 0 {
        let meta = result
            .as_object_mut()
            .expect("object")
            .entry("_meta")
            .or_insert_with(|| json!({}));
        let mcport = meta
            .as_object_mut()
            .expect("checked metadata")
            .entry("mcport")
            .or_insert_with(|| json!({}));
        let map = mcport.as_object_mut().expect("checked metadata");
        map.entry("assets")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("checked asset list")
            .extend(resources);
        if unavailable > 0 {
            map.insert("unavailable_asset_count".into(), json!(unavailable));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
    };
    #[test]
    fn exact_origin_and_literal_path_are_required() {
        let base = Url::parse("http://127.0.0.1:3845/mcp").unwrap();
        assert!(asset_url("http://127.0.0.1:3845/assets/image-1.svg", &base).is_some());
        for raw in [
            "http://localhost:3845/assets/image.svg",
            "http://127.0.0.1:9999/assets/image.svg",
            "https://127.0.0.1:3845/assets/image.svg",
            "http://127.0.0.1:3845/assets/../secret",
            "http://127.0.0.1:3845/assets/%2e%2e/secret",
            "http://127.0.0.1:3845/assets/a%2fb",
            "http://127.0.0.1:3845/assets/a%5cb",
            "http://127.0.0.1:3845/assets/x?url=/secret",
            "http://127.0.0.1:3845/assets/x#fragment",
            "file:///etc/passwd",
            "http://127.0.0.1:3845/admin",
            "http://user:pass@127.0.0.1:3845/assets/image.svg",
        ] {
            assert!(asset_url(raw, &base).is_none(), "accepted {raw}")
        }
    }
    #[test]
    fn only_literal_returned_links_are_candidates() {
        let base = Url::parse("http://127.0.0.1:3845/mcp").unwrap();
        let mut found = BTreeSet::new();
        candidates(
            &json!({"text":"Use ![icon](http://127.0.0.1:3845/assets/icon.svg) and file:///etc/passwd","data":"http://127.0.0.1:3845/assets/not-a-link.png"}),
            &base,
            &mut found,
            0,
        );
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec!["http://127.0.0.1:3845/assets/icon.svg"]
        )
    }
    #[test]
    fn sentence_punctuation_does_not_become_a_filename() {
        let base = Url::parse("http://127.0.0.1:3845/mcp").unwrap();
        let mut found = BTreeSet::new();
        candidates(
            &json!({"text":"Preview http://127.0.0.1:3845/assets/pixel.png. Next: http://127.0.0.1:3845/assets/card.svg;"}),
            &base,
            &mut found,
            0,
        );
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec![
                "http://127.0.0.1:3845/assets/card.svg",
                "http://127.0.0.1:3845/assets/pixel.png"
            ]
        );
    }
    #[tokio::test]
    async fn fetches_exact_returned_asset_and_does_not_follow_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/assets/icon.svg",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer selected-account");
                    ([("content-type", "image/svg+xml")], "<svg/>").into_response()
                }),
            )
            .route(
                "/assets/redirect.png",
                get(|| async { (StatusCode::FOUND, [("location", "/private")], "") }),
            )
            .route(
                "/private",
                get(|| async {
                    panic!("redirect was followed");
                    #[allow(unreachable_code)]
                    "private"
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = Endpoint::Http {
            url: format!("{origin}/mcp"),
            headers: Default::default(),
            bearer_token: Some("selected-account".into()),
        };
        let original = json!({"content":[{"type":"text","text":format!("![image]({origin}/assets/icon.svg) {origin}/assets/redirect.png")}],"structuredContent":{"keep":"original"}});
        let mut result = original.clone();
        materialize(
            &endpoint,
            &mut result,
            &CancellationToken::new(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(result["content"], original["content"]);
        assert_eq!(result["structuredContent"], original["structuredContent"]);
        assert_eq!(
            result
                .pointer("/_meta/mcport/assets")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            result.pointer("/_meta/mcport/assets/0/blob").unwrap(),
            "PHN2Zy8+"
        );
        assert_eq!(
            result
                .pointer("/_meta/mcport/unavailable_asset_count")
                .unwrap(),
            1
        );
        server.abort();
    }
    #[tokio::test]
    async fn no_filesystem_fetch_or_cancelled_fetch() {
        let endpoint = Endpoint::http("http://127.0.0.1:1/mcp");
        let mut result = json!({"content":[{"type":"resource_link","uri":"file:///etc/passwd"}]});
        let original = result.clone();
        materialize(
            &endpoint,
            &mut result,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(result, original);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        result = json!({"content":[{"type":"resource_link","uri":"http://127.0.0.1:1/assets/image.png"}]});
        let original = result.clone();
        materialize(&endpoint, &mut result, &cancelled, Duration::from_secs(1)).await;
        assert_eq!(result, original);
    }
}
