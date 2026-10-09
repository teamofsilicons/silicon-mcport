# Native backend candidate and installation

The `Native backend candidate` workflow builds and tests the Rust server natively
on ARM64 inside a pinned Amazon Linux 2023 image. It checks required glibc symbols
against 2.34 and starts/stops the real executable using a disposable data directory.
The web job separately tests and builds the website. Assembly requires the same
source revision and server checksum and emits an immutable `.tar.gz` plus SHA-256.
The server embeds its full source revision and exposes it in `/health`; both
native smoke and target installation require that exact revision and version.
The bundle includes the server, website, systemd unit, proxy example, installer,
license, build provenance and a checksum for every file. It contains no runtime
credentials, database, encryption key or deployment settings.

This server bundle is separate from the `mcport` CLI packages distributed through
Silicon Apps. No host, DNS name, runtime credentials, production upload or
deployment has been selected or performed by this workflow. `/health` proves
process/version readiness; it does not prove Silicon Accounts sign-in, provider
calls, mail delivery or telemetry ingestion.

## Prepare and inspect

Dispatch `.github/workflows/backend.yml` at the exact committed revision, wait for
all three jobs and download `mcport-backend-candidate`. Confirm the run's head SHA
and the downloaded archive's sidecar digest. Use this installer from the reviewed
source revision; do not execute code from an unverified archive.

```sh
python3 deploy/install.py --bundle /path/to/mcport-backend-REV-linux-aarch64.tar.gz \
  --sha256 EXPECTED_SHA256 --revision FULL_40_CHARACTER_REVISION
```

Without `--apply`, this only verifies paths, modes, architecture, build evidence,
source revision and the complete payload checksum inventory. It writes nothing
and can run on the developer's Mac. Native loader/systemd/configuration checks
are performed on the selected host during installation.

Use an ARM64 Linux host compatible with Amazon Linux 2023 and glibc 2.34 or newer,
Python 3, systemd and the normal glibc/libgcc runtime. Prefer the established
Silicon SSM-managed host convention, limited inbound TCP80/443, encrypted persistent
storage and an unprivileged `mcport` account. CI has no AWS or application secrets.
Provision the host and runtime configuration separately after target selection.

Prepare `/var/lib/mcport` owned by `mcport`, mode0700; `/etc/mcport/runtime.env`
owned by root, mode0600; and the `mcport` user/group. Populate the environment from
the deployment secret store, using `environment.example` in the source checkout
as the field reference. Set the exact external HTTPS origin, `ACCOUNTS_URL`, the
`mcport` app secret from Silicon Accounts (`MCPORT_APP_SECRET`), the webhook secret
(`MCPORT_ACCOUNTS_WEBHOOK_SECRET`) and environment-specific delivery/telemetry keys.
The service refuses to start, with a message naming the variable, when the app
secret is missing or an Accounts URL is not https.
The installer requires explicit `MCPORT_DATA_DIR=/var/lib/mcport` and
`MCPORT_BIND=127.0.0.1:4380` so its backup covers the service's actual state.
Never use fixture values or copy another application's credentials. Preserve the
master key with its database across every upgrade. A fresh deployment must begin
with an empty data directory.

For acceptance, run a separate MCPort backend wired to a test Silicon Accounts
deployment; see [testing setup](testing.md). One backend has no testing
environments, and production never supplies a credential for testing.

## Silicon Accounts setup

In Silicon Accounts, the `mcport` app needs: `device_flow` and `public_client` on
(the CLI signs Carbons in with the device flow and exchanges Silicons' short-lived
tokens without a secret); the website's `/auth/callback` in `redirect_uris` and its
origin in `allowed_origins`; no email or phone fields. Point the app webhook at
`https://backend.mcport.teamofsilicons.com/webhooks/accounts` (every update) and
put its `whsec_` secret in `MCPORT_ACCOUNTS_WEBHOOK_SECRET`; `POST
/v1/apps/mcport/webhook/test` must then show a delivered `ping`. Provider OAuth uses
the backend's own `/oauth/callback`; never register it in Silicon Accounts.

Moving a backend that ran a release before 0.3.0 to Silicon Accounts is a one-time
cutover with `mcport-server link-identities`; follow
[the cutover runbook](../docs/migration/cutover.md).

The service binds `127.0.0.1:4380`, serves the API (the website is a separate
deployment) and retains state under `/var/lib/mcport`. Adapt `Caddyfile.example`
to the selected hostname, validate it with `caddy validate`, then configure
DNS/HTTPS and the loopback proxy before the health-gated cutover. Do not add an
unrelated public listener for port4380.

## Install a reviewed candidate

Run on the prepared target through its existing administration mechanism:

```sh
sudo python3 deploy/install.py \
  --bundle /protected/mcport-backend-REV-linux-aarch64.tar.gz \
  --sha256 EXPECTED_SHA256 --revision FULL_40_CHARACTER_REVISION \
  --public-health-url https://SELECTED_HOST/health --apply
```

Only `--apply` changes the host. The installer uses a deployment lock, refuses
existing immutable release paths, checks loader dependencies, stops the existing
service, copies its state and runtime file into a root-only backup, then switches
`/opt/mcport/current` and the service unit. It verifies matching private/public
health versions before enabling the unit. It does not alter runtime credentials,
Caddy, DNS, firewall rules or cloud infrastructure. Existing systemd drop-ins
are refused because they can override the validated data location or executable.
Retain its backup receipt.
Run one gateway instance per database; this release has no distributed leases.

After installing, separately prove real Carbon and Silicon sign-in, a useful shared
and local-host provider call, permission revocation, authorized downloads, a
delivered Silicon Accounts webhook, and mail/telemetry delivery where enabled. Check `journalctl -u
mcport.service` without copying secrets or provider payloads into public logs.

## Backup, failed cutover and recovery

Backups are under `/var/backups/mcport/TIMESTAMP-REVISION`, mode0700. They include
the stopped data directory, matching runtime configuration, previous unit and a
receipt identifying the old release. The entire data directory is copied while
the service is stopped, including SQLite WAL files and any generated encryption
key. Do not copy a running SQLite database or restore only its main `.sqlite`
file. Copy verified backups to the selected encrypted off-host store; host-local
backups do not protect against disk or host loss. Set retention and test recovery
on an isolated machine before production use.

If failure occurs before the release switch, the previous active service resumes.
After a failed switch, the installer restores the previous release link and unit
but keeps the service stopped. If stopping cannot be confirmed, it preserves the
candidate link/unit and reports the unresolved writer; do not restore data until
the service is actually stopped. Starting a new executable may have changed data;
blindly starting the older executable is unsafe. No automatic database rollback
or deletion is performed. Before recovery, inspect the protected receipt, select
the matching database/key/configuration backup and preserve the failed data for
diagnosis. With the service confirmed stopped, move the failed data directory
aside, restore the complete backup as `/var/lib/mcport`, set ownership recursively
to `mcport:mcport` and keep its top-level mode0700. Restore the matching protected
runtime file as root0600, previous unit and release symlink, then run
`systemctl daemon-reload` and start `mcport.service`. Verify both health/version
and real login/provider authorization again. Restoring a previous session/token
snapshot may require users to sign in again; never restore over active writers.

An initial install has no previous release to restore. On its failure the service
stays stopped; fix the target configuration or deploy a newly reviewed revision.
No archive or installer test is evidence that a production cutover succeeded.
