# Development and deployment

## Local work

```sh
cargo fmt --all --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
python3 -m unittest discover -s scripts/tests -p 'test_*.py'    # packaging, backend bundle and catalog scripts
python3 tests/e2e/run.py                                       # the real binaries end to end (fake Silicon Accounts)
python3 scripts/check.py                                       # all of the above, the discovery commands and the web
```

The Rust tests need no database or network: the service uses temporary SQLite stores and in-process fixtures (a local
Ed25519 JWKS for Silicon Accounts tokens, MCP providers, Postmark), the CLI tests run the real `mcport` binary against a
stub Silicon Accounts and backend, and the client tests cover the device flow, short-lived token exchange, refresh
rotation and the stored sign-in. `mcport-mcp` transport tests need Python 3 (`MCPORT_TEST_PYTHON=/abs/python3`).
The end-to-end journey runs the real CLI, host daemon and service against a loopback fake Silicon Accounts and MCP
fixtures; [tests/e2e/README.md](../tests/e2e/README.md) describes it and the scenarios against a real local stack.

To run the whole thing on this machine, start a local Silicon Accounts stack (the silicon-accounts testkit) that has
an `mcport` app with `device_flow` and `public_client` on, then:

```sh
MCPORT_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh --build   # service 127.0.0.1:4241, MCP fixtures 4242
export MCPORT_URL=http://127.0.0.1:4241 ACCOUNTS_URL=http://localhost:9590
cargo run -p mcport-cli -- login
scripts/dev-accounts-stop.sh
```

`dev-accounts.sh` is idempotent: it points mcport's webhook at the service with the app's credentials, keeps the
signing secret in `.local/dev-accounts/webhook-secret` (0600, never committed) and proves a signed test delivery
before it reports ready. `python3 scripts/dev_accounts.py --help` lists its settings (another stack file, port block,
state directory, binary). It talks only to a Silicon Accounts on this machine. Against any other deployment, start
`mcport-server` with the variables below.

When the public Silicon Accounts URL (the token issuer) and the address the service should call differ, set
`ACCOUNTS_API_URL` as well. The service no longer serves a website; the website is deployed on its own and calls the
same API.

## Service configuration

Copy `deploy/environment.example` into a protected service environment file and supply secrets from your deployment's
secret store.

| Variable | Purpose |
|---|---|
| `ACCOUNTS_URL` | Silicon Accounts' public URL; access tokens must carry it as `iss`. HTTPS, or HTTP on this machine. |
| `ACCOUNTS_API_URL` | Optional: where the service calls Silicon Accounts, when that differs from `ACCOUNTS_URL`. |
| `MCPORT_APP_ID`, `MCPORT_APP_SECRET` | The app at Silicon Accounts (`mcport`) and its secret (service only; required). |
| `MCPORT_ACCOUNTS_WEBHOOK_SECRET` | The `whsec_` secret of mcport's Silicon Accounts webhook; without it the webhook answers 503. |
| `MCPORT_PUBLIC_URL`, `MCPORT_WEB_URL` | External service and website origins (provider OAuth callbacks, download links). |
| `MCPORT_BIND`, `MCPORT_DATA_DIR` | Listener and protected persistent storage. |
| `MCPORT_MASTER_KEY` | Optional 32-byte key as 64 hex characters; otherwise generated once in the data directory. |
| `MCPORT_ALLOWED_UPSTREAM_ORIGINS` | Explicit operator-only exceptions for controlled private HTTP origins. |
| `POSTMARK_SERVER_TOKEN`, `MCPORT_REPORT_FROM` | Bug-report delivery. |
| `MCPORT_TELEMETRY_KEY`, `MCPORT_TELEMETRY_URL` | Space Station telemetry. |

Variables of releases before 0.3.0 are ignored with a warning naming each. Put HTTPS in front of the listener and pass
only trusted forwarding metadata. `/health` reports liveness and the contract version; it does not check Silicon
Accounts or providers.

Never reuse a master key across deployments or lose it during an upgrade: database and key are backed up and restored
together. The service is a single instance; several replicas would need shared leases and refresh coordination.
[deploy/README.md](../deploy/README.md) covers installation and [deploy/testing.md](../deploy/testing.md) a separate test
deployment.

## Silicon Accounts setup

The `mcport` app's sign-in setup at Silicon Accounts needs `device_flow` (the CLI's `mcport login`), `public_client`
(the CLI exchanges Silicons' short-lived tokens and refreshes with `client_id` alone), the website's
`/auth/callback` in `redirect_uris`, and only the profile. Its webhook points at
`https://backend.mcport.teamofsilicons.com/webhooks/accounts`. MCPort accepts no proofs from other apps yet; the scopes
`mcport.connections.read` and `mcport.tools.call` are reserved. The [cutover runbook](migration/cutover.md) has the
exact calls.

## Releases

The CLI ships through Silicon Apps (`silicon-apps install mcport`); the Silicon Apps daemon updates installed apps, and
MCPort has no updater of its own. A release is one `.tar.gz` per target with an `apps.yaml` that lists only that target,
built by `scripts/package-apps.sh <version> <target> <binary>`: it checks the binary, runs `mcport --help`, `mcport
accounts --json` and `mcport login status --json` signed out in an empty home whenever the machine can run it, then
`silicon-apps validate` and `silicon-apps pack`. Linux packages are static musl builds (linked with Zig), so one binary
runs on every distribution. The release workflow (`.github/workflows/release.yml`, on a `v<version>` tag) builds and
checks every target and uploads the archives as the artifact `mcport-silicon-apps-release`; uploading them to Silicon
Apps is an operator step. Only the four Linux validation workers are live today, so the macOS and Windows archives are
kept for later. Development releases install as `mcport>dev`. [scripts/README.md](../scripts/README.md) has the
commands.

Publish the crates in dependency order: `mcport-core`, `mcport-mcp`, `mcport-api`, `mcport-daemon`, `mcport-client`,
then `mcport-cli`. 0.3.0 changes the identity types and removes the old sign-in calls, so it is a breaking release for
every crate.

Before a public release, install it fresh on representative targets, sign in as a Carbon (device flow) and a Silicon
(short-lived token), and make a useful call through a cloud and a local MCP.
