# MCPort development

## How the pieces fit

- `mcport-core`: wire types shared by the service and its clients.
- `mcport-api`: the stateless HTTP client for `/api/v1`. Every request carries an explicit Silicon Accounts access
  token for the `mcport` app (`Authorization: Bearer`); nothing is retried, refreshed or read from disk implicitly.
- `mcport-client`: the package Rust applications use. It re-exports `mcport-api` and adds, behind features, sign-in with
  Silicon Accounts (`accounts`, default), one stored sign-in with single-flight refresh (`session`) and explicit local
  host registries plus the embedded host daemon (`local`).
- `mcport-daemon`: the outbound host connector embedded in the CLI (`mcport daemon run`). It long-polls the service with
  its host token, runs only connections registered in its local registry and journals every job.
- `mcport-mcp`: MCP sessions over streamable HTTP and stdio.
- `mcport-cli`: the `mcport` command. It owns arguments, prompts, output and the files under the home; everything else
  goes through `mcport-client`, so the CLI has no capability the package lacks.
- `mcport-server`: the service (not published).

## Signing in

MCPort is a public client at Silicon Accounts: the CLI holds no secret.

- Carbons: `POST {ACCOUNTS_URL}/v1/device/authorize` with `client_id=mcport`, then poll `POST /v1/oauth/token` with
  `grant_type=urn:ietf:params:oauth:grant-type:device_code`, waiting `interval` seconds and 5 more after each
  `slow_down`, until approved, `access_denied` or `expired_token` (10 minutes).
- Silicons: `POST /v1/oauth/token` with `grant_type=urn:silicon:params:oauth:grant-type:slt`, the short-lived token and
  `client_id=mcport`. `invalid_grant` refusals are classified (already used, expired, wrong app, unknown, the minting
  sign-in ended) and never echo the token.
- Refresh: `grant_type=refresh_token` with `client_id` alone. Refresh tokens rotate and a used one ends the whole
  sign-in, so the CLI refreshes under an exclusive lock on `<sign-in>.lock`, re-reads the file after taking it, and
  writes the new pair (temporary file, fsync, rename) before using it. It refreshes when less than 60 seconds are left,
  and repeats a command once when the service answers 401 `token_expired` or `signed_out` (a 401 means nothing ran).
  If Silicon Accounts is briefly unreachable, a token with time left is still used.
- Sign-out: `POST /v1/oauth/revoke` with `token`, `token_type_hint=refresh_token` and `client_id`. Signing in again in
  the same home revokes the sign-in it replaces.
- Before spending a code or token, `mcport login` reads the backend's `GET /api/v1/discovery` and refuses when the
  backend trusts another Silicon Accounts or app id (`accounts_mismatch`, `app_id_mismatch`).

The service verifies access tokens locally against the Accounts JWKS (`aud` = `mcport`, `iss` = `ACCOUNTS_URL`),
introspects them on sensitive routes and applies Silicon Accounts webhooks (sign-outs, removed access, id and custodian
changes). The [API contract](../../../docs/API.md) has the details.

## Files

Under `${SILICON_HOME:-$HOME}/.mcport/dir` (directories 0700, files 0600, symbolic links refused):

- `settings.json`: telemetry, backend and Silicon Accounts URLs.
- `accounts/<key>.json` (+ `.lock`): the sign-in for one backend (`<key>` = SHA-256 of `["<backend>",null]`): the
  account (uuid, id, kind, display name, custodian), the access and refresh tokens, their expiry, the Silicon Accounts
  URL and app id that issued them.
- `contexts/<key>/hosts/<SHA-256 of ["<host id>",null]>/registry.json`: a host's registry (host token, registered
  endpoints, local provider credentials) with its daemon's lock, status, journal and log. Version 2 keys the owner and
  personal provider accounts by Silicon Accounts uuid; version 1 (mcport 0.2 and earlier) keyed them by old ids and
  keeps running until `mcport host migrate` rewrites it (`registry.v1.json` keeps the original).
- `sessions/` holds sign-ins from mcport 0.2 and earlier. They are no longer used; `mcport logout` removes the file.

`mcport accounts --json`, `mcport --help`, `mcport docs` and, signed out, `mcport login status --json` start no async
runtime, need no network or existing home, and write nothing, so they pass in the Silicon Apps validation sandbox.

## Work from source

```sh
git clone https://github.com/teamofsilicons/silicon-mcport
cd silicon-mcport
cargo build --locked -p mcport-cli -p mcport-server
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

The CLI's tests run the real binary against a stub Silicon Accounts and backend (`crates/mcport-cli/tests`); the client's
tests cover the device flow, token exchange, refresh rotation and the session file (`crates/mcport-client/tests`).

To run everything locally, start a Silicon Accounts deployment of your own with an `mcport` app that has `device_flow`
and `public_client` turned on, then the service:

```sh
ACCOUNTS_URL=http://localhost:9590 MCPORT_APP_SECRET=<the app's secret> \
  MCPORT_ACCOUNTS_WEBHOOK_SECRET=<its whsec_ secret> cargo run -p mcport-server   # serves 127.0.0.1:4380
export MCPORT_URL=http://127.0.0.1:4380 ACCOUNTS_URL=http://localhost:9590
mcport login                     # or: silicon-accounts login --app mcport -q | mcport login --slt-stdin
```

`deploy/environment.example` lists every service setting. Keep the app secret, webhook secret, encryption key, Postmark
token and telemetry key out of repositories and client packages.

## Results and retries

The client keeps complete MCP result JSON and tells a tool's `isError` apart from service errors. A transport failure
during a change reports `outcome_unknown`: inspect activity before trying again. The daemon journals before dispatch,
never runs a provider action twice after an unknown outcome, and retries only the upload of a finished result.
Downloads re-check access and tool policies; the CLI writes new owner-only files and never overwrites one.

## Releases

MCPort ships through Silicon Apps: `silicon-apps install mcport`, and the Silicon Apps daemon keeps installed apps
current. Each release is one archive per target with `apps.yaml` at its root; development releases install as
`mcport>dev`. The CLI has no updater of its own and installs no system service; the host daemon is started by `mcport
host new` and `mcport daemon start`.

The crates are published in dependency order: `mcport-core`, `mcport-mcp`, `mcport-api`, `mcport-daemon`,
`mcport-client`, then `mcport-cli`. 0.3.0 changes the identity types and removes the old sign-in calls, so it is a
breaking release.

The service is a single instance; several replicas would need shared leases and refresh coordination.

Read usage with `mcport docs usage`, command syntax with `mcport <command> --help`, and the repository's `docs/` for the
architecture and the API.
