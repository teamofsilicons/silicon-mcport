# mcport-client

The primary Rust package for Silicon MCPort. The CLI uses this package for gateway calls, local connection/account configuration and host execution.

The default feature set exposes a stateless HTTP `Client`. It never discovers files or environment variables, stores tokens, logs in or refreshes implicitly, or retries a tool invocation. Backend URLs require HTTPS except for loopback development.

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

Apply `.testing(environment_id)` to every test request context, including login and refresh: `RequestContext::default().testing(environment_id)` before login, then `RequestContext::authenticated(token).testing(environment_id)` for authenticated requests. Caller-owned storage must keep backend, environment, principal and organization credentials separate. Serialize refresh and atomically persist the new token pair before releasing that lock.

Provider account authorization is separate from IAM application login. `Client::connect_account` saves a remote provider grant; `authorize_account` returns the browser consent URL. Local credentials stay in an explicitly supplied execution-host registry through the `local` feature.

Methods cover configuration, access, provider accounts, tool policies, tools/resources/prompts/completions, hosts, activity, cancellation, result assets, settings, reports and bounded telemetry. Results preserve complete JSON and unknown fields. `result.isError` is an MCP failure even when HTTP succeeded. `tool` traverses discovery pages; other paged methods expose `nextCursor` unchanged.

`assets` lists embedded result parts; `download_asset` returns at most 16 MiB, never writes a file or follows a provider URL. Current access is checked on every download. Mutation transport errors report `outcome_unknown`: inspect activity before retrying. `rpc` accepts an explicit idempotency key. Custom HTTP clients must disable credential-forwarding redirects and mutation retries.

## Local execution

Enable `mcport-client = { version = "0.1.0", features = ["local"] }` for host operations. Every filesystem path, executable, authenticated identity and environment is explicit. The package does not discover a home or session.

- `registry_for_host` maps a gateway host registration into a local `Registry`.
- `register_connection` validates ownership/scope and maps a connection to its local endpoint, preserving existing accounts. `unregister_connection` removes it.
- `connect_account`, `disconnect_account` and `account_status` enforce shared-owner or per-user selection. Credentials are not uploaded or provider-verified by these helpers.
- `lock_registry` serializes caller-owned load/mutate/save sequences. Configuration helpers mutate only the supplied in-memory registry; the caller explicitly calls `Registry::save`.
- `run` embeds the outbound connector in the current process with an explicit `CancellationToken`. `start` accepts an absolute MCPort CLI executable; `status` and `stop` operate only on the supplied host directory.

Use a dedicated, trusted directory per host, containing `registry.json`; never replace its host identity while its daemon is running. The supplied base directory is trusted. Control-file symlinks, a symlinked host directory and symlinks in the CLI's `.mcport` subtree are rejected. Status/stop also verify the running host identity. Registry files contain credentials and use owner-only Unix permissions; native Windows ACL behavior needs platform verification.

```rust,ignore
use mcport_client::local::{self, Registry, Scope};
use std::{collections::BTreeMap, path::Path};

// connection and session came from authenticated gateway responses.
let scope = Scope {
    backend_url: client.backend_url(),
    environment: session.environment.clone(),
    principal_id: session.actor.principal_id.clone(),
    org_id: session.actor.org_id.clone(),
};
let path = Path::new("/trusted/mcport-host/registry.json");
let guard = local::lock_registry(path)?;
let mut registry = Registry::load(path)?;
local::register_connection(&mut registry, &connection, &scope, BTreeMap::new())?;
local::connect_account(&mut registry, &connection, &scope,
    serde_json::json!({"kind":"bearer", "secret": provider_token}))?;
registry.save(path)?;
drop(guard);
local::run(path, local::CancellationToken::new()).await?;
```

A local `Scope` is authenticated metadata supplied by the caller, not a credential or a substitute for gateway authorization. The CLI owns argument parsing, prompts and home/session persistence; these explicit library operations are available to other Rust applications.

`host_poll`, `host_result` and `host_progress` use a separate explicit `HostContext { host_id, host_token, environment, isi }`. The daemon uses the same underlying `mcport-api` transport, avoiding a dependency cycle. The runtime owns the durable journal and retries only idempotent result upload, never the provider action.

Run `cargo test -p mcport-api -p mcport-client --all-features`. See the [API contract](../../docs/API.md), [architecture](../../docs/architecture.md) and [fixture regression](../../tests/e2e/README.md).
