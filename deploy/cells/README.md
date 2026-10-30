# MCPort on Silicon Cells

Run MCPort's Rust backend as a Cells persistent service with the cell's PostgreSQL
database. The existing `api.mcport.teamofsilicons.com` host remains a thin HTTPS
proxy so installed clients, local host daemons, provider OAuth callbacks and
Accounts webhooks keep their addresses. It holds no application records and runs
no MCP tools after cutover. The website remains on Vercel.

## Runtime and authentication

The service uses the runtime's `CELLS_DATABASE_URL` and outbound HTTP(S) proxy.
Do not override `CELLS_*`, proxy settings or the database gate, publish container
ports, mount the host filesystem, or install production registry credentials.
The guest uses UID10001, a read-only root and bounded temporary storage. The
runtime owns its lifecycle. PostgreSQL is durable; `/tmp/mcport` is scratch only.

Cells consumes `Authorization` as its own session credential. The edge therefore
overwrites `X-Mcport-Authorization` with the incoming bearer, removes upstream
`Authorization`, and injects the cell's **publishable** API key. The service
validates Cells' request context and then performs its normal Accounts or host
token checks on the transported bearer. The custom header grants no authority by
itself. Public health/discovery, OAuth callbacks and signed Accounts webhooks
continue to use their existing application rules. An Accounts app link to the
cell is unnecessary for this direct Accounts-token integration.

Use [Caddyfile.example](Caddyfile.example) as the replacement for only the current
API site. Keep the website, legacy retirement response and unrelated hosts intact.
Keep the Cells hostname for TLS/SNI/Host and preserve the original request path
after the `/services/mcport` prefix. Caddy automatically handles upgrades and
streams. Both the Cells runtime and Caddy need the bounded 630-second response
header deadline to preserve McPort's maximum 600-second MCP call. The initial
Cells 30-second deadline cannot serve a long synchronous tool call. McPort's
local host long-poll returns within 20 seconds.

The cell and service must allow `https://mcport.teamofsilicons.com` as an origin.
Never strip Origin merely to evade an origin policy. Existing local results can
be 16MiB and McPort admits 20MiB requests. The Cells deployment must support the
bounded 20MiB service ingress limit before deploying McPort. The initial Cells
0.1.0 8MiB service limit is not
sufficient. Do not silently lower McPort's existing result limits.

Run `python3 deploy/cells/check-edge.py --caddy /absolute/path/to/caddy` to verify
the exact bridge against a disposable HTTPS upstream. It checks spoofed-header
replacement, missing authentication, verified upstream TLS, raw body preservation
and the routed path/query without using any real credentials or remote service.

## Image preparation

[The workflow](../../.github/workflows/cells.yml) builds the selected Git commit on
native `ubuntu-24.04-arm`, runs the server/core/MCP tests plus the explicitly
enabled PostgreSQL importer/runtime test against a disposable PostgreSQL16
database, checks execution with a
read-only root and no network, then publishes `ghcr.io/teamofsilicons/mcport-server`.
The builder is official Rust1.98 and the runtime is Debian Bookworm; both base
indexes are digest-pinned. Runtime apt package resolution can change, so the
resulting manifest digest and workflow receipt identify the deployable artifact.
The image contains source-built executable bytes and system certificates, never
app secrets, encryption keys, local data or author sessions.

The workflow's `MCPORT_CELLS_IMAGE_RECEIPT` log/summary records source revision,
manifest/config/layer digests and build-input hashes. Select its exact digest,
never a mutable tag. A new GHCR package defaults private. Use GitHub's ordinary
package settings to make this reviewed application image public, then verify an
anonymous pull using an empty Docker configuration. Keep the registry allowlist
unchanged. No Cells deployment happens in this workflow.

For a local build, export only the frozen tracked source:

```sh
git archive HEAD | tar -x -C /absolute/path/to/empty-build-context
docker build --platform linux/arm64 \
  -f /absolute/path/to/empty-build-context/deploy/cells/Dockerfile \
  --build-arg SOURCE_REVISION="$(git rev-parse HEAD)" \
  -t mcport-cells:review /absolute/path/to/empty-build-context
```

## Author and secrets

Use `si:saket-agents` and the shared CLI home
`/Users/codanium/.config/silicon/agent/home`. Keep the cell creation receipt private:
it contains the database password and secret API key. Grant any additional
authors through the ordinary Cells API; do not borrow another person's session.

Preserve the existing McPort encryption key exactly when importing its encrypted
records. Encode its original 32bytes as 64hex characters in `MCPORT_MASTER_KEY`.
Put it and `MCPORT_APP_SECRET`, `MCPORT_ACCOUNTS_WEBHOOK_SECRET`, and any
configured `POSTMARK_SERVER_TOKEN` in Cells Secrets through file/stdin or in-memory
API calls. Never put values in service definitions, image layers, process
arguments, logs or Git. Keep the existing `MCPORT_REPORT_FROM` if configured.
Store only the publishable cell key in the edge's protected environment file.

[service.example.json](service.example.json) contains only public settings. Select
the verified manifest digest before deployment. The service name is `mcport`.
Wait for the actual running instance and verify its health/source revision; an
accepted deploy response alone is not readiness.

## Migration and cutover gates

1. Verify current AWS account and regions. The October30 audit found account
   `234951665042`, McPort host `i-0bf2c2f54fce6cfca` in `us-east-1` and Cells host
   `i-02816b0a44b95a2e9` in `us-east-2`. Re-read current state before operating.
2. Retain a protected backup of the current Caddy configuration, runtime settings,
   encryption key and complete SQLite database. The current Accounts namespace is
   `/var/lib/mcport-accounts`; its systemd unit is `mcport-accounts`. Preserve every
   table, including allocation/replay/account/webhook ledgers. Do not touch the
   separate legacy namespace.
3. Rehearse `mcport-server import-postgres --source /private/snapshot.sqlite`
   against a disposable database; it previews by default, and `--apply` commits.
   It reads `DATABASE_URL` and `MCPORT_MASTER_KEY` privately from its environment.
   Use certificate-verified TLS for the public Cells database endpoint; plaintext
   connections are only allowed to the literal-loopback service gate or fixtures.
   Verify record counts, encrypted bytes, public-ID ledger, replay links and key
   consistency. Do not improvise
   SQL exports that omit identity tables or generate a replacement key.
4. Close public writes with a temporary maintenance response, drain executing
   calls and leased local jobs, then stop the old writer. Take the final coherent
   backup and import that exact snapshot. Do not run both services as writers.
5. Start the Cells service using the imported database and original key. Through
   a protected staging route, verify real account sign-in, remote MCP discovery
   and calls, local host reconnect/call/result, provider credential lifecycle,
   replay, permissions, revoked access, webhooks and the full supported result
   size. Verify the expected persisted records after a service restart.
6. Adapt/validate the complete proposed Caddy configuration before reload. Switch
   only the API site. Recheck public health/discovery, genuine browser/CLI flows,
   OAuth callback origin, webhook delivery and a local-host call through the real
   public address. Keep the stopped legacy data and final backup recoverable.
7. Replace old SQLite backup scheduling with verified Cells database recovery and
   a separate protected copy of required application secrets. Record exact source,
   image, service version, cell, migration proof, tests and live cutover evidence.

Before public writes reach Cells, rollback can restore the original proxy and
restart the untouched original SQLite writer. **After Cells accepts writes,
restarting the old SQLite database loses data and risks repeated provider
actions.** Put the API in maintenance and use a verified reverse migration or
coordinated database/application restore. Never roll back only a binary, erase
replay ledgers or run two independent writers to shorten an outage.
