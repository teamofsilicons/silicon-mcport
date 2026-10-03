# Development and deployment

## Local work

```sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
npm ci --prefix web
npm run build --prefix web
npm exec --prefix web -- vitest run
python3 tests/e2e/run.py
```

The real-binary integration harness creates independent backend/IAM/provider services, homes and databases. See its README for port and cleanup behavior. Manual evidence is separate from automated assertions. Never call fixture tests proof of live IAM or production provider consent.

`cargo run -p mcport-server` serves the API at `127.0.0.1:4380`. `npm run dev --prefix web` serves port 4381 and proxies `/api`. A production frontend build at `web/dist` is served by the backend with browser history fallback. Run the server from the repository/deployment root or provide that directory layout.

## Backend configuration

Copy `deploy/environment.example` into a protected service environment file and supply secrets through your deployment's secret store. Important settings:

| Variable | Purpose |
|---|---|
| `MCPORT_APP_ID`, `MCPORT_APP_SECRET` | Registered Honeycomb application and backend-only IAM credential |
| `MCPORT_IAM_URL`, `MCPORT_IAM_WEB_URL` | IAM API and login website |
| `MCPORT_PUBLIC_URL`, `MCPORT_WEB_URL` | Exact external backend/frontend origins and callbacks |
| `MCPORT_BIND`, `MCPORT_DATA_DIR` | Listener and protected persistent storage |
| `MCPORT_MASTER_KEY` | Optional 32-byte key as 64 hex characters; otherwise generated once in the data directory |
| `MCPORT_WEBHOOK_SECRET`, `MCPORT_WEBHOOK_SECRET_VERSION` | Separate IAM signing secret and version |
| `MCPORT_LIFECYCLE_SECRET` | Dedicated Honeycomb participant service token, at least 32 characters |
| `MCPORT_TEST_APP_SECRETS` | JSON map from test environment UUID to its imported application secret |
| `MCPORT_ALLOWED_UPSTREAM_ORIGINS` | Explicit operator-only exceptions for controlled private HTTP origins |
| `POSTMARK_SERVER_TOKEN`, `MCPORT_REPORT_FROM` | Bug-report delivery configuration |
| `MCPORT_TELEMETRY_KEY`, `MCPORT_TELEMETRY_URL` | Space Station production table key and service |
| `MCPORT_TEST_TELEMETRY_KEYS` | JSON map of separate test environment table keys |

Use HTTPS at the reverse proxy, preserve multiple Set-Cookie headers, and pass only trusted forwarding metadata. Restrict the control-plane service token to Honeycomb. Health responds at `/health`; health does not verify IAM/provider workflows.

Do not reuse a master key across unrelated deployments or lose it during an upgrade. Database and encryption-key backup/restore must be tested together. The current backend is a single-instance service; a multi-instance deployment requires transactional distributed session/lease coordination.

## Honeycomb and releases

Follow the [official ready-application guide](https://docs.honeycomb.teamofsilicons.com/guides/team-of-silicons-ready-applications/). Register `mcport` under its owning organization; request only identity/membership fields required by the real login and resource policy. Register typed website login callbacks, `/webhooks/iam`, and the authenticated testing participant path documented in API.md. Production registration is owned by Honeycomb.

The application currently exposes no OBO or ATA receiver endpoints. Do not publish catalog declarations that imply delegated capabilities without implementing their verification and resource policy. An IAM application identity key does not become user authority.

`honeycomb.yaml` describes all six native CLI targets. The release workflow builds on native runners, packages deterministic inputs, and uploads a candidate artifact. Validate its actual archive using the installed Honeycomb CLI before upload. The CLI includes its daemon in the same executable.

Publish dependency crates in order: `mcport-core`, `mcport-mcp` and `mcport-api`, then `mcport-daemon`, `mcport-client`, and `mcport-cli` only if distributing the CLI through crates.io. The client defaults to the HTTP API; enable its `local` feature for the host/configuration facade. Use versioned registry dependencies in release packages. A successful macOS build does not validate Linux/Windows installation or ABI compatibility. Run a fresh install, real IAM login, tool discovery and a useful call for representative native platforms before public release.

The checkout's repository/docs/package URLs are intended distribution metadata. Confirm the actual repositories, hosting and registry entries exist before publication. Publishing and approval completion require separate observed evidence; an uploaded candidate is not automatically public.
