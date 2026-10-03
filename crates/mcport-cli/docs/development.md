# MCPort development

The CLI uses `mcport-client` for all MCPort operations. The package re-exports the stateless gateway API and exposes local registration, account validation and explicit host run/start/stop/status helpers through its `local` feature. The CLI owns arguments, prompts, output, home selection and session persistence. Callers of the Rust package supply identity, environment, paths and executables explicitly. The daemon uses the underlying `mcport-api` transport for host polling, progress and durable result acknowledgements, avoiding a dependency cycle; local MCP traffic belongs to the protocol runtime.

## Work from source

```sh
git clone https://github.com/teamofsilicons/silicon-mcport
cd silicon-mcport
cargo build --locked -p mcport-cli -p mcport-server
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
npm ci --prefix web
npm test --prefix web
npm run build --prefix web
python3 tests/e2e/run.py
```

The repository/package URLs are distribution metadata; check their availability for the version you are using. `scripts/check.py` combines Rust, web, packaging and fixture gates after installing `scripts/requirements.txt` in a private Python environment. The real-binary regression starts isolated loopback IAM/MCP fixtures and fresh homes; it does not bypass application authorization or use live email credentials. Fixtures are not proof of live provider compatibility.

Run `cargo run -p mcport-server` from the repository root to serve port 4380. `npm run dev --prefix web` serves port 4381 and proxies `/api`. Production `web/dist` is served by the gateway. The server requires a registered IAM application credential; development without live credentials uses `python3 tests/e2e/serve.py`, whose printed tokens are fixture-only.

## Configuration and isolation

Use `deploy/environment.example` as the backend configuration reference. Keep the IAM application secret, separate webhook and lifecycle secrets, encryption key, Postmark token and Space Station keys outside repositories and client packages. The backend stores encrypted record bodies; database backups and their encryption key must be recovered together.

Application login exchanges an IAM app-bound SLT. Provider OAuth or bearer credentials are a separate authority. User sessions are bound to principal, organization, backend and environment. Host tokens are bound to one registered host and environment. A remote caller never supplies a local command or endpoint in a gateway job.

A validated `--test` context has separate identities, provider accounts, hosts, jobs and data. Honeycomb prepare/disable/restore/clean/delete lifecycle changes fence stale work by generation. Production credentials are never a fallback. Live provisioning requires the registered application and coordinated lifecycle receipts; a fixture pass does not create a live test environment.

## Results and retries

The SDK preserves complete MCP result JSON and distinguishes a tool's `isError` from gateway errors. A transport failure can leave the outcome unknown. The SDK never retries requests automatically. The daemon journals before dispatch, prevents duplicate provider execution and retries only acknowledgement of an already completed job. Cancellation is best effort and cannot undo a completed upstream effect.

Downloads recheck current connection/tool permissions, and the CLI creates new private files without overwriting existing files. Local materialized links remain bounded by the runtime's origin/path rules. Unsupported interactive provider capabilities return explicit errors; do not treat them as implicit permission to execute a different action.

## Release work

The native workflow builds and tests Linux, macOS and Windows for x86-64 and ARM64. Stage ZIPs preserve executable modes; the final Honeycomb archive is `.tar.gz` with a root manifest and six target payloads. Package validation and checksums do not prove all native platforms ran: inspect each native job and test fresh installations.

Production registration, secrets, HTTPS hosting, current Honeycomb authority, publication reviews and crates.io distribution are operator steps. The source workflow creates candidate artifacts only. Organization/access-key/API-key runtime contexts are not implied by Carbon/Silicon login support; use only credential modes documented for the deployed backend.

The current gateway is a single-instance service. Horizontal replicas require distributed transaction, refresh and lease coordination. The native systemd unit expects `/opt/mcport/current` and protected `/var/lib/mcport` data. Linux GNU artifacts use Ubuntu 24.04 as their tested runtime baseline; older distributions and Windows permission behavior require native verification.

Read current usage with `mcport docs usage`, command syntax with `mcport <service> --help`, and the online development/API documentation linked by `mcport --help`.
