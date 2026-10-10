# MCPort cutover: Silicon IAM and Honeycomb → Silicon Accounts and Silicon Apps

For the operator moving production MCPort to 0.3.0: the service (backend
`https://backend.mcport.teamofsilicons.com`, EC2 `i-0bf2c2f54fce6cfca`, SQLite in `/var/lib/mcport`), the website
(`https://mcport.teamofsilicons.com`, Vercel) and the CLI (Honeycomb today, Silicon Apps after). Nothing here has been run
against production. Every command that touches production is marked `# run at cutover`; run none of them before the
day, and none without the review this runbook asks for. The service steps were rehearsed on 2026-10-10 against a copy
of a database written by MCPort 0.2.0 and the local test Accounts stack, the CLI steps with real sign-ins and a copy of
a 0.2.0 host registry, and the packaging with real builds (see [progress.md](progress.md)).

## Order and dependencies

- **Other apps.** MCPort calls no other app and no app calls it: it has no proofs to issue or accept, no Ting
  delivery and no Silicon Interface calls (the survey found none). It can cut over on its own day, before or after the
  other seven apps; nothing else must switch at the same moment.
- **Platform.** Production Silicon Accounts (the `mcport` app exists there) and Silicon Apps (the four Linux validation
  workers are live; macOS and Windows have none yet, checked 2026-10-10 with `GET /v1/capabilities`).
- **Within MCPort**, the three parts switch together, in this order, because no old client works with the new service
  and no new client works with the old one: service → website → CLI release promoted to production. Keep the gap
  between them to minutes; announce a short window to MCPort's Carbons.
- **Fleet.** The Silicon runtime (stemcell `silicon connect`) still runs `mcport login <token>` and `mcport iam --json`.
  Both keep working in 0.3.0 (positional login is an alias of `--slt`; `iam --json` is a hidden alias of
  `accounts --json`, kept for one minor release), but the token must now be a Silicon Accounts short-lived token
  (`slt_…`, from `silicon-accounts login --app mcport -q`). An IAM-era `oac_…` code is refused before anything is sent.
  Until the runtime mints Accounts tokens, each Silicon signs in itself (see "CLI and host daemons").
- **Ting.** None: MCPort sends nothing through Ting. Space Station telemetry is unchanged.

## Before the day

1. **Silicon Accounts app `mcport`** (exists in production). Sign-in setup: `device_flow: true`, `public_client: true`,
   `redirect_uris` = the website's `/auth/callback`, `allowed_origins` = the website origin, no email or phone fields.
   Read it first; arrays replace, so send the lists back whole with the version you read:

   ```sh
   # run at cutover (or the day before): app credentials from Secrets Manager silicon-mcport/production-runtime
   curl -s -u "mcport:$MCPORT_APP_SECRET" https://accounts.teamofsilicons.com/v1/apps/mcport \
     | jq '{config_version, redirect_uris: .signin_config.redirect_uris, allowed_origins: .signin_config.allowed_origins,
            device_flow: .signin_config.device_flow, public_client: .signin_config.public_client, webhook}'
   curl -s -X PATCH -u "mcport:$MCPORT_APP_SECRET" https://accounts.teamofsilicons.com/v1/apps/mcport/signin-config \
     -H 'Content-Type: application/json' -H 'Idempotency-Key: mcport-cutover-signin-1' \
     -d '{"expected_version": <config_version>, "device_flow": true, "public_client": true, "required_fields": [],
          "redirect_uris": [<every uri you read>, "https://mcport.teamofsilicons.com/auth/callback"],
          "allowed_origins": [<every origin you read>, "https://mcport.teamofsilicons.com"]}'
   ```

   Webhook secret: generate it now, without a URL, so the service can hold it before it starts; the URL is set in
   step 4 of the cutover, once the 0.3.0 service answers at it (a later `PUT` keeps this secret). Store the answer's
   `whsec_…` as `MCPORT_ACCOUNTS_WEBHOOK_SECRET` (next section). Do not register the service's `/oauth/callback` with
   Silicon Accounts (that is provider OAuth).

   ```sh
   # run at cutover (or the day before)
   curl -s -X POST -u "mcport:$MCPORT_APP_SECRET" https://accounts.teamofsilicons.com/v1/apps/mcport/webhook/generate-secret \
     -H 'Idempotency-Key: mcport-cutover-webhook-secret-1'
   ```
2. **Accounts for every user.** Each Carbon and Silicon that used MCPort needs a Silicon Accounts account, and each
   Silicon its custodian set. Known: `c:saket` → `zQo`.
3. **Mapping file.** On a copy of the production database (never the live file), list the ids to map:

   ```sh
   # on a copy, off the production host
   MCPORT_DATA_DIR=/tmp/mcport-copy MCPORT_APP_SECRET=<accounts app secret> mcport-server legacy-principals
   ```

   Write `mapping.csv` with the header `iam_principal_id,accounts_uuid` (optional third column `iam_public_id`) and one
   line per principal: the principal's IAM id (`c:…`/`si:…` as `legacy-principals` prints it) and the uuid of the same
   Carbon or Silicon at Silicon Accounts (its profile, or `GET /v1/accounts/by-id/{id}` with the app credentials).
   uuids are case-sensitive. Principals left out keep their records unlinked (invisible) and are listed in the report;
   they can be added in a later run.
4. **Rehearse** on that copy: `mcport-server link-identities --file mapping.csv --dry-run`, then without `--dry-run`, then
   once more (the third run must report only `already_linked`/`unmapped`). Review `unmapped_principals`, `renamed`,
   `duplicates_kept_unlinked`, `cutover_grants` and `evidence_without_mapping`.
5. **Service bundle.** Dispatch `.github/workflows/backend.yml` at the reviewed revision and download
   `mcport-backend-candidate` ([deploy/README.md](../../deploy/README.md)). The bundle no longer carries the website.
6. **CLI packages.** Push the `v0.3.0` tag at the same revision (it must equal the CLI version); the release workflow
   builds and checks every target and uploads `mcport-silicon-apps-release`. Download it, then check it:

   ```sh
   sha256sum -c SHA256SUMS
   for f in mcport-0.3.0-*.tar.gz; do d="check/${f%.tar.gz}"; mkdir -p "$d" && tar -xzf "$f" -C "$d" && silicon-apps validate "$d"; done
   cat checks/*.json      # each binary's three commands passed on a runner that could execute it
   ```

   Then, as an author of `mcport` at Silicon Apps, make sure the app has a listing: `silicon-apps setup mcport show`.
   If it has none yet, save its details and access (`silicon-apps setup mcport details --description-file
   description.txt --tags tools,mcp`, `silicon-apps setup mcport access --visibility public`), following the publishing
   guide on developers.teamofsilicons.com. Upload the four Linux archives and create a **development** release:

   ```sh
   # run at cutover (or the day before): a development release reaches only `mcport>dev` installs
   silicon-apps upload mcport --target linux-x86_64 mcport-0.3.0-linux-x86_64.tar.gz
   silicon-apps upload mcport --target linux-i686 mcport-0.3.0-linux-i686.tar.gz
   silicon-apps upload mcport --target linux-aarch64 mcport-0.3.0-linux-aarch64.tar.gz
   silicon-apps upload mcport --target linux-armv7hf mcport-0.3.0-linux-armv7hf.tar.gz
   silicon-apps packages mcport            # each package passed its worker's three commands
   silicon-apps release mcport --version 0.3.0 --package <id> --package <id> --package <id> --package <id>
   ```

   The macOS and Windows archives stay in the artifact until their workers are live. Meanwhile a Carbon on a Mac (for
   example one hosting Figma desktop for its Silicons) installs the macOS archive from that artifact through Silicon
   Apps, which registers it for updates once a macOS release exists:
   `silicon-apps install mcport --archive mcport-0.3.0-macos-aarch64.tar.gz --sha256 <its line in SHA256SUMS>`; or
   `cargo install --locked mcport-cli` after the crates are published (dependency order: `mcport-core`, `mcport-mcp`,
   `mcport-api`, `mcport-daemon`, `mcport-client`, `mcport-cli`).
7. **Website.** The Next.js website (the `web/` of this release) builds with `pnpm build`. Prepare the Vercel project's
   production environment from [deploy/vercel.md](../../deploy/vercel.md) (`APP_ID`, `APP_SECRET`, `ACCOUNTS_URL`,
   `APP_API_URL`, `PUBLIC_URL`, `SESSION_SECRET`), keep the old `MCPORT_BACKEND_ORIGIN` until the switch, and do not
   promote a deployment yet.
8. **Clients.** Installed `mcport` 0.1.x/0.2.x CLIs cannot sign in to 0.3.0 (their sign-in routes answer 410
   `client_update_required`, naming `silicon-apps install mcport` and the new sign-in commands). Tell MCPort's Carbons
   and the custodians of its Silicons the date and what each will run (see "CLI and host daemons").

## Runtime environment

`/etc/mcport/runtime.env` already exists (`runtime_from_secret.py` only creates it). First keep the IAM-era file, which
a rollback to 0.2.x needs (the installer's backup holds whatever the file contains when the installer runs):
`sudo cp -p /etc/mcport/runtime.env /var/backups/mcport/runtime.env.0.2` (`# run at cutover`). Then edit it as root
(mode 0600) before installing, so the 0.3.0 service starts with the right secrets:

- set `MCPORT_APP_SECRET` to the **Silicon Accounts** `mcport` app secret (it currently holds the IAM app secret);
- add `ACCOUNTS_URL="https://accounts.teamofsilicons.com"` and `MCPORT_ACCOUNTS_WEBHOOK_SECRET="whsec_…"`;
- remove `MCPORT_IAM_URL`, `MCPORT_IAM_WEB_URL`, `MCPORT_WEBHOOK_SECRET`, `MCPORT_WEBHOOK_SECRET_VERSION`,
  `MCPORT_LIFECYCLE_SECRET`, `MCPORT_TEST_APP_SECRETS`, `MCPORT_TEST_TELEMETRY_KEYS` (0.3.0 ignores them with a
  warning).

Update the Secrets Manager object `silicon-mcport/production-runtime` to match (only the keys
`runtime_from_secret.py` accepts: `MCPORT_APP_SECRET`, `MCPORT_ACCOUNTS_WEBHOOK_SECRET`, `ACCOUNTS_API_URL`,
`MCPORT_TELEMETRY_KEY`, `POSTMARK_SERVER_TOKEN`, `MCPORT_REPORT_FROM`, `MCPORT_MASTER_KEY`). The same app secret goes
into Vercel as `APP_SECRET`; the session secret is new and lives only in Vercel.

## The cutover

1. **Service.** Install the reviewed 0.3.0 bundle (it stops the old service, backs up the stopped data directory, key
   and runtime file, switches the release and health-checks it):

   ```sh
   # run at cutover, on the host through SSM
   sudo python3 /opt/mcport/bootstrap/install.py --bundle "$MCPORT_BUNDLE" --sha256 "$MCPORT_BUNDLE_SHA256" \
     --revision "$MCPORT_REVISION" --public-health-url https://backend.mcport.teamofsilicons.com/health --apply
   ```

   The new service starts on unlinked data: old records stay invisible until step 3, and the schema step it applies
   only adds tables and columns (0.2.x binaries can still read the database).
2. Stop it: `sudo systemctl stop mcport.service` (`# run at cutover`).
3. **Re-key**, as the service user with the service's environment (the mapping file readable by `mcport`):

   ```sh
   # run at cutover
   sudo systemd-run --wait --pipe --uid=mcport --gid=mcport \
     --property=EnvironmentFile=/etc/mcport/runtime.env \
     --setenv=MCPORT_DATA_DIR=/var/lib/mcport \
     /opt/mcport/current/mcport-server link-identities --file /var/lib/mcport/mapping.csv --dry-run
   ```

   Compare the report with the rehearsal, then run it again without `--dry-run`. It runs in one transaction; a failure
   changes nothing. Uuids are checked against Silicon Accounts (lookups with the app secret); `--offline` skips that
   (then custodians are unknown and every principal with evidence of use outside the owner keeps an explicit grant).
4. Start it: `sudo systemctl start mcport.service` (`# run at cutover`). Check `/health` reports 0.3.0, point the
   Silicon Accounts webhook at it (the `PUT` keeps the secret generated before the day; its answer's `secret` is `null`
   for that reason), and send a test delivery: it must show as delivered (or `journalctl -u mcport.service` shows the
   ping). A `401` there means the runtime's `MCPORT_ACCOUNTS_WEBHOOK_SECRET` is not that secret.

   ```sh
   # run at cutover
   curl -s https://backend.mcport.teamofsilicons.com/health
   curl -s -X PUT -u "mcport:$MCPORT_APP_SECRET" https://accounts.teamofsilicons.com/v1/apps/mcport/webhook \
     -H 'Content-Type: application/json' -H 'Idempotency-Key: mcport-cutover-webhook-1' \
     -d '{"url": "https://backend.mcport.teamofsilicons.com/webhooks/accounts", "events": null}'
   curl -s -X POST -u "mcport:$MCPORT_APP_SECRET" -H 'Idempotency-Key: mcport-cutover-ping-1' \
     https://accounts.teamofsilicons.com/v1/apps/mcport/webhook/test
   ```

5. **Website.** In the Vercel project, set the production environment from step 7 of "Before the day", delete
   `MCPORT_BACKEND_ORIGIN`, and promote the reviewed preview deployment of this revision to production
   (`# run at cutover`: `vercel promote <preview-deployment-url>`, or Promote to Production in the dashboard). Check that
   `https://mcport.teamofsilicons.com/auth/sign-in` redirects to `https://accounts.teamofsilicons.com/authorize?app_id=mcport&…`.
6. **CLI.** Promote the development release, so `silicon-apps install mcport` installs 0.3.0 and every Silicon Apps
   updater picks it up within a minute:

   ```sh
   # run at cutover
   silicon-apps releases mcport --channel development
   silicon-apps promote mcport <development-release-id> --version 0.3.0
   silicon-apps readiness mcport && silicon-apps publish mcport     # the first time only, if not yet published
   ```

7. **Prove** (each step on the real deployment): a Carbon signs in on the website and with `mcport login`, and sees its
   connections; one of its Silicons signs in with `silicon-accounts login --app mcport -q | mcport login --slt-stdin` and
   sees the Carbon's `circle` connections ("you and the Silicons you look after"); an invitee calls a shared connection; the Carbon's Mac
   daemon still runs a local call; `mcport --version` from a fresh `silicon-apps install mcport` on Linux prints
   `mcport 0.3.0`; a sign-out at Silicon Accounts makes the next sensitive call answer `sign_in_revoked`; a download from
   the website comes from a one-time ticket on the service.

## CLI and host daemons

0.3.0 CLIs sign in with Silicon Accounts only; nothing in the old CLI state is converted.

- **Carbons:** `mcport login` (device flow: approve the printed code at `https://accounts.teamofsilicons.com/device`).
  **Silicons:** `silicon-accounts login --app mcport -q | mcport login --slt-stdin`. The Silicon runtime's
  `mcport login <token>` (positional) and `mcport iam --json` keep working for one minor release: `iam --json` is a
  hidden alias that prints the `accounts --json` object.
- **Silicons still running the Honeycomb-installed CLI (0.1.x/0.2.x).** Honeycomb will not update them (MCPort ships
  no more Honeycomb releases), and the 0.3.0 service refuses them: their stored sessions answer 401 and their sign-in
  routes 410 `client_update_required`, whose recovery names `silicon-apps install mcport` and the new sign-in commands.
  On each Silicon (its custodian, or the Silicon on instruction):

  ```sh
  honeycomb uninstall mcport                 # the old copy, so it cannot shadow the new one on PATH
  silicon-apps install mcport                # into ${SILICON_HOME:-$HOME}/.apps/bin; kept current by the Apps updater
  command -v mcport && mcport --version      # .apps/bin/mcport, mcport 0.3.0
  silicon-accounts login --app mcport -q | mcport login --slt-stdin
  ```

  If the old copy cannot be removed, put `.apps/bin` first on `PATH`. Settings and host registries stay where they are
  (same paths under `$SILICON_HOME/.mcport/dir`).
- Sign-ins of 0.2 and earlier (`~/.mcport/dir/sessions/`) are ignored; `mcport login status --json` says
  `signed_in_before_silicon_accounts` and `mcport logout` deletes the old file.
- **Hosts registered before 0.3.0** keep serving local connections: the service sends their old account keys until the
  host's daemon reports `registry_version: 2`. A daemon of 0.2.x keeps running unchanged; once its machine has the 0.3.0
  CLI, its owner runs, on that machine and signed in as the owner:

  ```sh
  mcport daemon status                 # registry_version 1 and a "migrate" hint
  mcport host migrate <host> --dry-run
  mcport host migrate <host>           # keeps registry.v1.json; restarts the daemon if it ran
  ```

  Local provider accounts are re-keyed through `GET /api/v1/hosts/{host}/legacy-accounts`, which links each old key the
  daemon reported to the account `link-identities` mapped it to. If keys cannot be linked, the migration stops and names
  them: run the 0.3.0 daemon once on the old registry (`mcport daemon start`; it reports its keys within a poll) and
  retry, or pass `--drop-unmapped` and have those accounts reconnect with `mcport account connect`.
- Until a host is migrated, `account connect/disconnect/show` and `connection register` on its local connections answer
  `registry_not_migrated` with the two commands above; deleting a connection still removes it from the old registry.
- The Carbon's own daemon on this Mac (0.2.x, started from the old checkout) needs nothing until the Carbon installs the
  0.3.0 CLI and migrates its host.

## Rollback

- **Service, before step 4:** nothing has served linked data. Stop, restore the installer's backup (data directory and
  key together) and the previous release with the installer's documented recovery
  ([deploy/README.md](../../deploy/README.md)), and put back the IAM-era `runtime.env.0.2` kept before the edit. **After step 4**, a rollback means the same restore and losing what was
  created since; re-running `link-identities` with a corrected mapping (service stopped) is usually the better fix,
  since it recomputes every record from its preserved original.
- **Website:** Vercel's instant rollback to the previous static deployment, with `MCPORT_BACKEND_ORIGIN` restored. It
  works only together with a rolled-back service.
- **CLI:** `silicon-apps withdraw mcport <release-id> --reason "<one sentence>"` stops serving 0.3.0. There is no earlier
  Silicon Apps release to fall back to, so installs then fail with a clear error, and Silicons that need the old
  service reinstall the 0.2.x CLI with Honeycomb (`honeycomb install mcport`) until a fixed release ships.
- Silicon Accounts changes (the callback, origin and webhook) can stay: they do nothing for the old service.

## After

- Delete the mapping file from the data directory.
- Once every Silicon runs the Silicon Apps build, nothing calls the 410 routes; the next minor release removes them,
  the hidden `iam` alias and the host jobs' transition fields (after every host reports `registry_version: 2`).
- When IAM is retired, the IAM-era session and refresh rows (encrypted IAM refresh tokens) can be purged; 0.3.0 never
  reads them. No purge command exists yet; ask for one when needed.
