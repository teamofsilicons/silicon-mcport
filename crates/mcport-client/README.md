# mcport-client

The Rust package for Silicon MCPort. The `mcport` CLI is built on it, so everything the CLI does is available here.

- `Client` (always): stateless calls to MCPort's `/api/v1`. Each request carries an explicit Silicon Accounts access
  token for the `mcport` app in a `RequestContext`. Nothing is read from files or the environment, nothing is refreshed
  or retried implicitly, and backend URLs must be HTTPS (HTTP only on this machine).
- `accounts` (default feature): sign in as MCPort's public client at Silicon Accounts, with no secret. Device flow for
  Carbons, short-lived token exchange for Silicons, refresh and sign-out.
- `session`: keep one sign-in in one explicit file, refreshed single-flight.
- `local`: explicit host registries and the embedded host daemon.

```toml
mcport-client = { version = "0.3.0", features = ["session", "local"] }
```

## Sign in and call a tool

A Silicon hands over a short-lived token from `silicon-accounts login --app mcport -q`:

```rust,no_run
use mcport_client::accounts::{APP_ID, DEFAULT_ACCOUNTS_URL, SignIn};
use mcport_client::{Client, RequestContext};
use serde_json::json;

# async fn example(slt: &str) -> Result<(), Box<dyn std::error::Error>> {
let tokens = SignIn::new(DEFAULT_ACCOUNTS_URL, APP_ID)?.exchange_slt(slt).await?;
println!("signed in as {} ({})", tokens.account.id, tokens.account.uuid);
let client = Client::new("https://backend.mcport.teamofsilicons.com")?;
let context = RequestContext::authenticated(tokens.access_token.expose());
let result = client
    .call_tool(&context, "notes", "search", json!({"query": "release"}))
    .await?;
println!("{}", result.result);
# Ok(()) }
```

A Carbon approves a code instead:

```rust,no_run
use mcport_client::accounts::{APP_ID, DEFAULT_ACCOUNTS_URL, DeviceProgress, SignIn};

# async fn example() -> Result<(), mcport_client::accounts::SignInError> {
let sign_in = SignIn::new(DEFAULT_ACCOUNTS_URL, APP_ID)?;
let device = sign_in.start_device(Some("my tool on build-box")).await?;
println!("Open {} and confirm {}", device.verification_uri, device.user_code);
let tokens = sign_in
    .wait_for_device(&device, |progress| {
        if let DeviceProgress::SlowDown { interval } = progress {
            eprintln!("polling every {interval} s");
        }
    })
    .await?;
# let _ = tokens;
# Ok(()) }
```

`wait_for_device` honours `interval` and `slow_down` and stops when the code is approved, denied or expired (10 minutes).
Errors are `SignInError`s with a stable `code()`, a `message()` saying what failed and why, and a `hint()` saying what
to do; a refused short-lived token names its reason (`SltRefusal::AlreadyUsed`, `Expired`, `WrongApp`, `Unknown`,
`SignInEnded`). Tokens are `Secret`s: their `Debug` output hides them, and nothing here logs them.

## Keep a sign-in (`session`)

Refresh tokens rotate and a used one ends the whole sign-in, so refresh one at a time and store the new pair first.
`SessionFile` does both:

```rust,no_run
use mcport_client::session::{SessionFile, StoredSignIn};
use std::time::Duration;

# async fn example(sign_in: &mcport_client::accounts::SignIn, tokens: mcport_client::accounts::Tokens) -> Result<(), Box<dyn std::error::Error>> {
let file = SessionFile::new("/trusted/state/mcport-sign-in.json");
file.save(&StoredSignIn::new(sign_in, "https://backend.mcport.teamofsilicons.com", "slt", tokens))?;
// Later, in any process: a token valid for at least a minute, refreshed if needed.
let current = file.fresh(Duration::from_secs(60)).await?;
# let _ = current;
# Ok(()) }
```

The file is owner-only (0600 in a 0700 directory on Unix), written atomically and never a symbolic link. `fresh` takes
an exclusive lock on `<file>.lock`, re-reads the file and refreshes only if it is still needed. When the sign-in has
ended it removes the file and returns `SignInError::SignInEnded` with the reason. Sign out with
`SignIn::revoke(refresh_token)`.

## What the client covers

Connections, sharing (`grant_access` by `c:`/`si:` id, `allow` lists for Silicons), provider accounts (including a
Silicon's, for its custodian), tool policies, tools, resources, prompts, completions, hosts, activity, cancellation,
result assets and one-time download links, directory entries and their shares, settings, reports and telemetry.
`discovery()` needs no token; `me()` returns the signed-in account as MCPort sees it. Results keep complete JSON;
`result.isError` is a tool failure even when HTTP succeeded. `tool` walks discovery pages; other paged methods return
`nextCursor` unchanged.

Errors keep MCPort's `{"error":{"code","message","recovery"}}`: `code()`, `message()`, `recovery()`, `status()`,
`is_code("ambiguous_name")`; `needs_sign_in()` means get a new token rather than retry. A transport failure during a
change reports `outcome_unknown`: check activity before trying again. `download_asset` returns at most 16 MiB and never
follows a provider URL. Custom HTTP clients (`Client::with_http`) must not retry changes or forward credentials on
redirects.

## Local execution (`local`)

Every path, executable and identity is explicit; nothing discovers a home or sign-in.

- `registry_for_host` turns a host registration into a version 2 `Registry` owned by the signed-in account's uuid.
- `register_connection` / `unregister_connection` add or remove one connection's local endpoint, keeping existing
  credentials.
- `connect_account`, `disconnect_account` and `account_status` manage local provider accounts (the owner's shared one,
  or each caller's own, keyed by uuid). Credentials stay on the host.
- `lock_registry` serializes load/change/save; helpers only change the in-memory registry until `Registry::save`.
- `run` embeds the daemon in this process; `start` launches `mcport daemon run` from an absolute executable path;
  `status` and `stop` act on one host directory.
- Registries written by mcport 0.2 and earlier (version 1) keyed personal accounts by old ids. They still load and run,
  but account helpers refuse them with `Error::LegacyRegistry` until `migrate_registry` rewrites them with a map from
  old keys to uuids (the CLI's `mcport host migrate` gets that map from the service).

```rust,ignore
use mcport_client::local::{self, Registry, Scope};
use std::{collections::BTreeMap, path::Path};

// `connection` came from the service; `tokens` from a sign-in.
let scope = Scope { backend_url: client.backend_url(), account_uuid: tokens.account.uuid.clone() };
let path = Path::new("/trusted/mcport-host/registry.json");
let guard = local::lock_registry(path)?;
let mut registry = Registry::load(path)?;
local::register_connection(&mut registry, &connection, &scope, BTreeMap::new())?;
local::connect_account(&mut registry, &connection, &scope,
    serde_json::json!({"kind": "bearer", "secret": provider_token}))?;
registry.save(path)?;
drop(guard);
local::run(path, local::CancellationToken::new()).await?;
```

A `Scope` describes an account the service already authenticated; it is not a credential. Host calls (`host_poll`,
`host_result`, `host_progress`) use a separate `HostContext { host_id, host_token, isi }`.

Run `cargo test -p mcport-api -p mcport-client --all-features`. See the [API contract](../../docs/API.md) and the
[architecture](../../docs/architecture.md).
