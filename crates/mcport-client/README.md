# mcport-client

The public, stateless Rust client for Silicon MCPort. Backend URLs require HTTPS except for loopback development servers. It calls the same authenticated API as the CLI and website. It never reads files or environment variables, stores tokens, logs in implicitly, rotates a session implicitly, or retries a tool invocation.

```rust,no_run
use mcport_client::{Client, RequestContext};
use serde_json::json;

# async fn example() -> Result<(), mcport_client::Error> {
let client = Client::new("http://127.0.0.1:4380")?;
let session = client.login(&RequestContext::default(), "app-bound-slt").await?;
let context = RequestContext::authenticated(&session.access_token);
let tools = client.tools(&context, "notes", None).await?;
let result = client.call_tool(
    &context, "notes", "search", json!({"query":"release"}),
).await?;
println!("{}", result.result);
# Ok(()) }
```

Pass `RequestContext::testing(environment_id)` on every test request, including login and refresh. Caller-owned storage must keep backend, environment, principal and organization credentials separate. If storing refresh tokens, serialize refresh and atomically persist the new pair before releasing that lock.

Provider account authorization is distinct from MCPort's IAM application login. `connect_account` saves a manual remote grant; `authorize_account` returns the browser consent URL. Credentials for local MCPs stay with the execution host, outside this SDK.

Methods cover connection configuration, access, provider accounts, tool policies, MCP tools/resources/prompts/completions, hosts, activity, cancellation, result assets, settings, reports and bounded telemetry events. MCP results remain complete JSON, including unknown fields, text, media, resource links and structured content. `result.isError` is an MCP tool failure even when HTTP succeeded. `tool` traverses discovery pages; other paged list methods expose `nextCursor` unchanged.

`assets` lists downloadable embedded result parts; `download_asset` returns bytes (up to 16 MiB), never writes a file and never follows a provider URL. The API rechecks current access for every download.

Transport errors on mutations report `outcome_unknown`; inspect activity before deciding whether to retry. Supply an idempotency key through `rpc` for a logical invocation. Reusing that key with different content is rejected. A custom HTTP client must disable redirects that could forward credentials and must not retry mutations.

Run `cargo test -p mcport-client`. See the workspace [API contract](../../docs/API.md) and [fixture regression](../../tests/e2e/README.md).

The daemon also uses this package: `host_poll`, `host_result` and `host_progress` accept a separate explicit `HostContext { host_id, host_token, environment, isi }`. Polling is limited to 25 seconds and 4 MiB; result acknowledgements to 3 seconds and 64 KiB. No host credentials are stored in `Client`, no polling loop or result retry happens inside the SDK, and a test host never drops its environment header. The daemon owns the durable execution journal and retries only an idempotent completion upload, not the underlying provider action.
