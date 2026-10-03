use crate::McpError;
use bytes::Bytes;
use futures::{StreamExt, stream::BoxStream};
use http::{HeaderName, HeaderValue};
use rmcp::{
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::streamable_http_client::{
        StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
        StreamableHttpPostResponse,
    },
};
use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

type HttpError = StreamableHttpError<std::io::Error>;
fn protocol_error(message: &str) -> HttpError {
    StreamableHttpError::UnexpectedServerResponse(message.to_owned().into())
}

pub fn http_config(
    url: &str,
    headers: &BTreeMap<String, String>,
    bearer: Option<&str>,
    limit: usize,
) -> Result<StreamableHttpClientTransportConfig, McpError> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);
    config.max_sse_event_size = limit;
    config.reinit_on_expired_session = false;
    config.auth_header = bearer.map(str::to_owned);
    config.allow_stateless = true;
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| McpError::new("invalid_header", "An MCP header name is invalid."))?;
        if matches!(
            name.as_str(),
            "host"
                | "authorization"
                | "cookie"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "accept"
                | "content-type"
        ) || name.as_str().starts_with("mcp-")
        {
            return Err(McpError::new(
                "reserved_header",
                "MCP protocol and authorization headers cannot be overridden. Use the dedicated credential configuration.",
            ));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| McpError::new("invalid_header", "An MCP header value is invalid."))?;
        config.custom_headers.insert(name, value);
    }
    Ok(config)
}

#[derive(Clone)]
pub struct BoundedHttp {
    client: reqwest::Client,
    limit: usize,
}
impl BoundedHttp {
    pub fn new(client: reqwest::Client, limit: usize) -> Self {
        Self { client, limit }
    }
    fn request(
        &self,
        method: reqwest::Method,
        uri: &str,
        session: Option<&str>,
        auth: Option<&str>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, uri)
            .header("accept", "application/json, text/event-stream");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(auth) = auth {
            request = request.bearer_auth(auth);
        }
        request
    }
    async fn body(&self, response: reqwest::Response) -> Result<Vec<u8>, HttpError> {
        if response
            .content_length()
            .is_some_and(|size| size > self.limit as u64)
        {
            return Err(protocol_error(
                "MCP response exceeded the configured size limit",
            ));
        }
        let mut stream = bounded_bytes(response, self.limit);
        let mut data = Vec::new();
        while let Some(chunk) = stream.next().await {
            data.extend_from_slice(&chunk.map_err(StreamableHttpError::Io)?);
        }
        Ok(data)
    }
}

fn bounded_bytes(
    response: reqwest::Response,
    limit: usize,
) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    // Bound total bytes, including unterminated SSE lines, before parsing.
    let stream = async_stream::try_stream! {
        let mut source=response.bytes_stream(); let mut remaining=limit;
        while let Some(chunk)=source.next().await {
            let chunk=chunk.map_err(|_|std::io::Error::other("MCP HTTP response interrupted"))?;
            if chunk.len()>remaining { Err(std::io::Error::other("MCP response exceeds configured byte limit"))?; }
            remaining-=chunk.len(); yield chunk;
        }
    };
    stream.boxed()
}

impl StreamableHttpClient for BoundedHttp {
    type Error = std::io::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let response = self
            .request(
                reqwest::Method::POST,
                &uri,
                session.as_deref(),
                auth.as_deref(),
                headers,
            )
            .json(&message)
            .send()
            .await
            .map_err(|_| protocol_error("MCP HTTP request failed"))?;
        let status = response.status();
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status.as_u16() == 404 && session.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        if matches!(status.as_u16(), 401 | 403) {
            return Err(protocol_error(if status.as_u16() == 401 {
                "provider_authentication_required"
            } else {
                "provider_permission_denied"
            }));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|x| x.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|x| x.to_str().ok())
            .map(str::to_owned);
        if status.is_success() && content_type.starts_with("text/event-stream") {
            return Ok(StreamableHttpPostResponse::Sse(
                sse_stream::SseStream::from_bytes_stream(bounded_bytes(response, self.limit))
                    .boxed(),
                session,
            ));
        }
        let body = self.body(response).await?;
        if body.is_empty()
            && status.is_success()
            && !matches!(message, ClientJsonRpcMessage::Request(_))
        {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if !status.is_success() {
            // Some legacy servers reject discovery with a generic HTTP error.
            // Translate that safe discovery-only rejection; never replay actions.
            if let ClientJsonRpcMessage::Request(request) = &message {
                let value = serde_json::to_value(&message).unwrap_or_default();
                if value.get("method").and_then(|m| m.as_str()) == Some("server/discover")
                    && matches!(status.as_u16(), 400 | 404 | 405 | 406 | 415 | 422)
                {
                    let fallback = serde_json::json!({"jsonrpc":"2.0","id":request.id,"error":{"code":-32601,"message":"Legacy MCP discovery; use initialize"}});
                    return Ok(StreamableHttpPostResponse::Json(
                        serde_json::from_value(fallback)
                            .map_err(|_| protocol_error("Invalid legacy discovery response"))?,
                        None,
                    ));
                }
            }
        }
        if content_type.starts_with("application/json") {
            let parsed = serde_json::from_slice::<ServerJsonRpcMessage>(&body)
                .map_err(|_| protocol_error("MCP server returned invalid JSON-RPC"))?;
            if status.is_success() || matches!(parsed, ServerJsonRpcMessage::Error(_)) {
                return Ok(StreamableHttpPostResponse::Json(parsed, session));
            }
        }
        Err(protocol_error(
            "MCP server returned an unsupported HTTP status or content type",
        ))
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        let status = self
            .request(
                reqwest::Method::DELETE,
                &uri,
                Some(&session),
                auth.as_deref(),
                headers,
            )
            .send()
            .await
            .map_err(|_| protocol_error("MCP session close failed"))?
            .status();
        if status.is_success() || matches!(status.as_u16(), 404 | 405) {
            Ok(())
        } else {
            Err(protocol_error("MCP session close rejected"))
        }
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>, HttpError> {
        let mut request = self.request(
            reqwest::Method::GET,
            &uri,
            session.as_deref(),
            auth.as_deref(),
            headers,
        );
        if let Some(last) = last {
            request = request.header("last-event-id", last);
        }
        let response = request
            .send()
            .await
            .map_err(|_| protocol_error("MCP event stream failed"))?;
        if response.status().as_u16() == 405 {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(protocol_error("MCP event stream rejected"));
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|x| x.to_str().ok())
            .is_some_and(|x| x.starts_with("text/event-stream"))
        {
            return Err(protocol_error("MCP event stream has invalid content type"));
        }
        Ok(sse_stream::SseStream::from_bytes_stream(bounded_bytes(response, self.limit)).boxed())
    }
}

/// Bound an unterminated stdio frame before the SDK buffers it.
pub struct LimitedRead<R> {
    inner: R,
    current: usize,
    limit: usize,
}
impl<R> LimitedRead<R> {
    pub fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            current: 0,
            limit,
        }
    }
}
impl<R: AsyncRead + Unpin> AsyncRead for LimitedRead<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                for byte in &buf.filled()[before..] {
                    if *byte == b'\n' {
                        self.current = 0;
                    } else {
                        self.current += 1;
                        if self.current > self.limit {
                            return Poll::Ready(Err(std::io::Error::other(
                                "MCP stdio frame exceeded configured byte limit",
                            )));
                        }
                    }
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
