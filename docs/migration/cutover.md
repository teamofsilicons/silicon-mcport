# MCPort cutover: Silicon IAM → Silicon Accounts (service)

For the operator moving production MCPort (backend `https://backend.mcport.teamofsilicons.com`, EC2
`i-0bf2c2f54fce6cfca`, SQLite in `/var/lib/mcport`) to 0.3.0. Nothing here has been run against production; the
whole sequence was rehearsed on 2026-10-10 against a copy of a database written by MCPort 0.2.0 and the local test
Accounts stack (see progress.md). Later stages add the CLI, website and Silicon Apps steps.

## Before the day

1. **Silicon Accounts app `mcport`** (exists in production). Sign-in setup: `device_flow: true`,
   `public_client: true`, `redirect_uris` = the website's `/auth/callback`, `allowed_origins` = the website origin, no
   email/phone fields. Webhook: `PUT /v1/apps/mcport/webhook` with
   `https://backend.mcport.teamofsilicons.com/webhooks/accounts` and every update; keep the `whsec_` secret it returns.
   Do not register the backend's `/oauth/callback` (that is provider OAuth).
2. **Accounts for every user.** Each Carbon and Silicon that used MCPort needs a Silicon Accounts account, and each
   Silicon its custodian set. Known: `c:saket` → `zQo`.
3. **Mapping file.** On a copy of the production database (never the live file), list the ids to map:

   ```sh
   MCPORT_DATA_DIR=/tmp/mcport-copy MCPORT_APP_SECRET=<accounts app secret> mcport-server legacy-principals
   ```

   Write `mapping.csv` with the header `iam_principal_id,accounts_uuid` (optional third column `iam_public_id`) and one
   line per principal. uuids are case-sensitive. Principals left out keep their records unlinked (invisible) and are
   listed in the report; they can be added in a later run.
4. **Rehearse** on that copy: `mcport-server link-identities --file mapping.csv --dry-run`, then without `--dry-run`, then
   once more (the third run must report only `already_linked`/`unmapped`). Review `unmapped_principals`, `renamed`,
   `duplicates_kept_unlinked`, `cutover_grants` and `evidence_without_mapping`.
5. **Clients.** Installed `mcport` 0.1.x/0.2.x CLIs cannot sign in to 0.3.0 (their login routes answer 410 with
   instructions). Every user installs the Silicon Apps build (`silicon-apps install mcport`) and signs in again. Host
   daemons keep running jobs through the transition fields until their owners run `mcport host migrate` (CLI stage).
6. **Interface.** The survey found no Silicon Interface (or other app) calls to MCPort, so nothing else must switch to
   proofs at the same time. MCPort accepts no proofs in 0.3.0.

## Runtime environment

`/etc/mcport/runtime.env` already exists (`runtime_from_secret.py` only creates it). Edit it as root (mode 0600) after
the installer's backup, or before installing, keeping a copy:

- set `MCPORT_APP_SECRET` to the **Silicon Accounts** `mcport` app secret (it currently holds the IAM app secret);
- add `ACCOUNTS_URL="https://accounts.teamofsilicons.com"` and `MCPORT_ACCOUNTS_WEBHOOK_SECRET="whsec_…"`;
- remove `MCPORT_IAM_URL`, `MCPORT_IAM_WEB_URL`, `MCPORT_WEBHOOK_SECRET`, `MCPORT_WEBHOOK_SECRET_VERSION`,
  `MCPORT_LIFECYCLE_SECRET`, `MCPORT_TEST_APP_SECRETS`, `MCPORT_TEST_TELEMETRY_KEYS` (0.3.0 ignores them with a
  warning).

Update the Secrets Manager object `silicon-mcport/production-runtime` to match (only the keys
`runtime_from_secret.py` accepts).

## The cutover

1. Install the reviewed 0.3.0 backend bundle with `deploy/install.py --apply` (it stops the old service, backs up the
   stopped data directory, key and runtime file, switches the release and health-checks it). The new service starts
   on unlinked data: old records stay invisible until step 3, and the schema step it applies only adds tables and
   columns (0.2.x binaries can still read the database).
2. Stop it: `sudo systemctl stop mcport.service`.
3. Re-key, as the service user with the service's environment (the mapping file readable by `mcport`):

   ```sh
   sudo systemd-run --wait --pipe --uid=mcport --gid=mcport \
     --property=EnvironmentFile=/etc/mcport/runtime.env \
     --setenv=MCPORT_DATA_DIR=/var/lib/mcport \
     /opt/mcport/current/mcport-server link-identities --file /var/lib/mcport/mapping.csv --dry-run
   ```

   Compare the report with the rehearsal, then run it again without `--dry-run`. It runs in one transaction; a failure
   changes nothing. Uuids are checked against Silicon Accounts (lookups with the app secret); `--offline` skips that
   (then custodians are unknown and every principal with evidence of use outside the owner keeps an explicit grant).
4. Start it: `sudo systemctl start mcport.service`. Check `/health` reports 0.3.0, then
   `POST /v1/apps/mcport/webhook/test` in Silicon Accounts shows a delivered `ping` (or check the journal).
5. Prove: a Carbon signs in (website and `mcport login`), sees its connections; its Silicons see `circle` connections;
   a shared connection works for an invitee; the Carbon's Mac daemon still runs a local call; a sign-out in Silicon
   Accounts makes the next sensitive call answer `sign_in_revoked`.

## Rollback

Before step 4 nothing has served linked data: stop, restore the installer's backup (data directory, key and runtime
file together) and the previous release with the installer's documented recovery. After step 4, a rollback means the
same restore and losing what was created since; re-running `link-identities` with a corrected mapping (service stopped)
is usually the better fix, since it recomputes every record from its preserved original.

## After

- Delete the mapping file from the data directory.
- When IAM is retired, the IAM-era session and refresh rows (encrypted IAM refresh tokens) can be purged; 0.3.0 never
  reads them. No purge command exists yet; ask for one when needed.
