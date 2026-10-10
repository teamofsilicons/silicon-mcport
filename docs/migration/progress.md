# MCPort migration progress (Silicon Accounts + Silicon Apps)

Branch `migrate/accounts-apps-20261010`, based on `origin/main` (`db23952`). Each stage appends a dated section:
what it did, commits, test commands and results, what is left, gotchas. Decisions are in
[decisions.md](decisions.md); the proposed contract changes are in
[understanding-proposal.md](understanding-proposal.md).

## 2026-10-10 — Stage 1 (service): baseline before any change

Environment: Rust 1.98.0, Python 3.14.6, `CARGO_TARGET_DIR=$PWD/target/mig`, 96 GB free.
MCPort stores everything in one AES-GCM-encrypted SQLite file; it has no Postgres, Redis or Docker dependency, so
the brief's Postgres on 5460 is not used by this app.

| Command | Result at `db23952` |
|---|---|
| `cargo test --locked --workspace --no-fail-fast` | pass: 113 tests + 2 doctests (server 56, daemon 12+4+1, mcp 3+9, cli 8+4, client 9, api 7, core 0) |
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `python3 -I tests/e2e/run.py --no-build --run-dir .mig/e2e-baseline/run` (with `target/debug -> mig/debug`) | pass: 91 checks (IAM fixture, Honeycomb lifecycle, real binaries) |

Nothing failed or was skipped at the baseline. The server's 56 tests are: store 13, assets 10 (incl. 4 in
`assets::tests`), oauth 9, lifecycle 8, auth 6, hosts 4, directory 4, operations 3, public_ids 3, connections 2,
state 1, tls 1, health 1 (local status).

The baseline e2e run left a real origin/main database (`.mig/e2e-baseline/run/server/mcport.sqlite` + `master.key`,
snapshotted to `.mig/upgrade-fixture/`). It is used below to prove the upgrade path with the real binaries. Its
production rows: 9 connections, 34 calls, 4 credentials, 6 account epochs, 3 policies, 1 host, 1 report, 1 settings,
all in org `tos` for `c:owner` and `si:researcher`, plus IAM-era sessions/refresh families and one Honeycomb test
world.

## 2026-10-10 — Stage 1 (service): done

The Rust service authenticates and authorises with Silicon Accounts only; IAM, Honeycomb, organizations and testing
environments are gone from it. Decisions S1–S25 are in [decisions.md](decisions.md); the operator runbook is
[cutover.md](cutover.md); proposed contract text is in [understanding-proposal.md](understanding-proposal.md).

What changed (`crates/mcport-server` unless noted):
- `silicon-iam-client` removed (it was never vendored here); `silicon-accounts-client = "0.4.0"` added. `hmac`,
  `tower-http` dropped (no IAM webhook, CORS or static website); `clap` added for the operator commands.
- Config: `ACCOUNTS_URL`, optional `ACCOUNTS_API_URL`, `MCPORT_APP_ID`, required `MCPORT_APP_SECRET`,
  `MCPORT_ACCOUNTS_WEBHOOK_SECRET`; exact boot errors; http only for loopback; IAM-era variables ignored with warnings.
- `accounts.rs` (JWKS cache, introspection cache, lookups within budget, circle/custodian helpers), `auth.rs`
  (bearer extractors `Auth`/`Live`, revocation), `accounts_webhook.rs` (`POST /webhooks/accounts`, six events + ping),
  `allowances.rs` (Silicon allow list), `identity.rs` (`legacy-principals`, `link-identities`), `identity_store.rs`.
- Every handler re-keyed to account uuids; circle, custodian and invited access; sharing by id; directory shares;
  download tickets; host job transition fields; `lifecycle.rs` (Honeycomb + IAM webhook) deleted; `/api/v1/iam`,
  `/api/v1/auth/*`, `/webhooks/iam` answer 410.
- Schema step 1 (`PRAGMA user_version`): additive tables and `legacy_*` columns only. Stored records keep older fields.
- `mcport-core` 0.3.0 wire types; all crates 0.3.0. `mcport-api`, `mcport-client` (local registry helper), CLI and
  daemon got only build fixes (the CLI still speaks the IAM-era login, which 0.3.0 servers refuse).
- Docs/config: `docs/API.md` (contract 0.3.0), new `docs/openapi.yaml` (validated OpenAPI 3.1, 36 paths matching the
  router), `docs/architecture.md`, `deploy/README.md`, `deploy/aws/deploy.md`, `deploy/testing.md` (rewritten),
  `deploy/environment.example`, `deploy/aws/runtime_from_secret.py`, `scripts/backend_smoke.py`,
  `deploy/Caddyfile.example`; `honeycomb.yaml` version follows the workspace until the packaging stage replaces it.

Commits: `bc47e14` baseline record · `e5e49cd` service migration · `d8fd90a` report ordering · `66b9c36` API/config
docs · `fffbf60` exact-id resolution test · `43b9c2b` migration docs · the per-caller lookup limit and
ticket hardening (S26, S27) · (final progress update).

Tests (all with `CARGO_TARGET_DIR=target/mig`):

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --locked --workspace --no-fail-fast` | pass: 127 tests + 2 doctests (server 70, daemon 17, mcp 12, cli 12, client 9, api 7) |
| `python -m unittest discover -s scripts/tests` (venv with PyYAML 6.0.3) | 28 pass |
| `python -m unittest discover -s tests/e2e -p 'test_*.py'` | 4 pass (fixture self-tests) |
| `openapi-spec-validator` on `docs/openapi.yaml` + router/path comparison | valid; no missing or extra paths |

Baseline tests that no longer exist by name (all IAM, Honeycomb or testing-environment behaviour), and what covers
their ground now: the 8 `lifecycle::` tests (Honeycomb lifecycle and IAM webhook removed) → `accounts_webhook::tests`
(5: signatures, dedupe, ordering, custodian change, sign-outs, removed access, deletion); the 6 `auth::` IAM session
tests → 8 new `auth::tests` (local JWT verification, wrong aud/iss/exp/signature/kind, JWKS refetch limit, bearer only,
revocation, introspection, deferred work, removed routes); `state::test_credential_configuration…` →
`state::accounts_urls_require_https_except_on_this_machine`; renamed ports: `assets::downloads_recheck_…`,
`directory::` ×3, `hosts::host_tokens_cannot_cross_…`, `oauth::refresh_waiting_for_…`,
`connections::…exact_ids_win…` (now in `access_tests`). New: connection access (8, incl. custodian provider-account powers), host custodian/transition (2),
download tickets (incl. sign-out), the per-caller lookup limit, directory legacy read, schema migration (empty + origin/main schema), mapping parser,
`link-identities` (dry run, commit, idempotent re-run, other mapping, back again, duplicates).

Proofs with real binaries (`.mig/` in this worktree, never committed):
1. **Upgrade path on a real origin/main database** (the baseline e2e run's database): `mcport-server
   legacy-principals` listed `c:owner` and `si:researcher`; `link-identities --dry-run` changed nothing; applying it
   linked 9 connections, 34 calls, 4 credentials, 3 policies, 1 host, 1 report, 1 settings and turned the 8
   org-visible connections into `circle` (no cutover grants needed: the only other user is the owner's Silicon, as a
   live custodian lookup confirmed); a re-run reported only `already_linked`. The 0.2.0 binary still opens a database
   after the 0.3.0 schema step.
2. **Against the shared local Accounts stack** (`ACCOUNTS_URL=http://localhost:9590`, API `127.0.0.1:9589`, service on
   `127.0.0.1:4241`, identities `c:mcport-owner-87242` = `2Cd`, `si:mcport-researcher-87242` = `EVW` in its care,
   `c:mcport-outsider-87242` = `hfF`, mapped onto that migrated database): real tokens (hosted-page code exchange for
   the Carbons, SLT exchange for the Silicon) were accepted; the owner saw its 9 connections, the Silicon the 8 circle
   ones, the outsider none; the custodian saw the Silicon's 25 calls; host creation passed live introspection;
   sharing with the Silicon answered `silicon_not_reachable` until the custodian allowed the outsider; real webhook
   deliveries applied an id change, a custodian transfer (activity moved to the new custodian at once), an STK
   rotation (`signed_out` for the old token) and removed access (`signed_out`; a new sign-in reactivated the account);
   the stack recorded every delivery as `delivered`.
3. Health and graceful SIGTERM with an unreachable Accounts URL and a dummy secret; exact boot errors for a missing
   app secret and a non-loopback `http://` Accounts URL.

Left for later stages:
- **CLI / client crate:** device-flow `mcport login`, `--slt`/`--slt-stdin` (and positional SLT alias), token store
  with single-flight refresh, `logout` revoke, offline `accounts --json` and `login status --json`, hidden `iam --json`;
  remove `Session`, gateway login/refresh/status/logout from `mcport-api`; `mcport allow …`, `--account` flags,
  `directory share`; registry v2 keyed by uuid with `mcport host migrate`, and the daemon reporting
  `registry_version: 2` and dropping its org check; bundled docs.
- **e2e harness** (`tests/e2e/run.py`, `serve.py`, `fixtures.py`): still IAM-based, so `scripts/check.py`'s e2e step
  fails against 0.3.0 until it moves to an Accounts fixture (the CLI must exist first).
- **Packaging / deploy:** `apps.yaml` + `release.yml` + `scripts/package.py` (Honeycomb → Silicon Apps); drop
  `web/dist` from `scripts/package_backend.py`, `deploy/install.py` and `backend.yml` (the service no longer serves it);
  `deploy/mcport.service` description; `deploy/vercel.md`.
- **Docs:** `README.md`, `docs/usage.md`, `docs/development.md`, crate READMEs and `crates/mcport-cli/docs` still
  describe IAM/Honeycomb; move `docs/release-readiness.md`, `docs/testing/` evidence to `docs/history/`.
- **Website:** Next.js BFF against this API (bearer tokens, download tickets, `/api/v1/me`, allow list).
- **Contract:** paste `understanding-proposal.md` into `UNDERSTANDING.md` (Carbon only).

Blocked on: nothing.

Gotchas:
- A worktree reads git excludes from the shared repository, so `.mig/` shows as untracked; stage explicit paths.
- `cargo test` and `cargo check` do not rebuild `target/mig/debug/mcport-server`; run `cargo build -p mcport-server`
  before using the binary (a stale 0.2.0 binary ignores arguments and serves on 4380).
- The local stack issues tokens with `iss` `http://localhost:9590` but is reached at `127.0.0.1:9589`: set both
  `ACCOUNTS_URL` and `ACCOUNTS_API_URL`.
- mcport's webhook on the local stack points at `http://127.0.0.1:4241/webhooks/accounts`; its secret is in
  `.mig/accept/webhook-secret` (0600). `test-stack.json` has no seeded webhook secret for mcport.
- `target/debug` is a symlink to `target/mig/debug` (for the e2e harness's hard-coded path).

## 2026-10-10 — Stage 2 (client crate and CLI): done

The `mcport` CLI and the `mcport-client` package sign in with Silicon Accounts only and follow the Silicon Apps CLI
contract. Decisions C1–C22 are in [decisions.md](decisions.md); the CLI part of the operator runbook is "CLI and host
daemons" in [cutover.md](cutover.md); contract text additions are at the end of
[understanding-proposal.md](understanding-proposal.md).

This stage was resumed after an interrupted first attempt that had left uncommitted work (and a server and fixture
process from 05:40, which were stopped). The work was reviewed, finished, re-tested and committed here.

What changed:
- `mcport-client`: `accounts` feature (default) — `SignIn` with the device flow (interval and `slow_down` honoured,
  10-minute expiry, progress callback), short-lived token exchange with `client_id` alone, refresh, RFC 7009 revoke,
  `SignInError` with stable codes, messages and hints, `SltRefusal` reasons, `Secret` tokens; `session` feature —
  `SessionFile` (0600 in 0700, atomic writes, symbolic links refused, single-flight refresh under `<file>.lock`,
  deletes the file when the sign-in ended); `local` — `Scope { backend_url, account_uuid }`, registry v2,
  `migrate_registry`, `Error::LegacyRegistry`.
- `mcport-api`: gateway `login/refresh/status/logout/iam`, `RequestContext.test_id` and `HostContext.environment`
  removed; `discovery`, `me`, allow list, directory shares, `account_for`/`disconnect_account_for`, `asset_ticket`,
  `legacy_host_accounts`, `canonical_backend_url`; error helpers (`code`, `message`, `recovery`, `status`, `is_code`,
  `needs_sign_in`). `mcport-core`: `Session` removed, `LegacyHostAccounts`, `ApiError.recovery` also reads `hint`.
- `mcport-daemon`: registry v2 with a v1 loader, `registry_version` reported, organization check removed, session
  pool and health keyed by (connection, account key).
- `mcport-cli`: `login` (device flow, `--open`, `--label`, `--json` progress lines), `login --slt/--slt-stdin/<token>`,
  `login status [--offline]`, `logout`, `accounts --json`, hidden `iam --json`, `allow`, `directory share/unshare/access`,
  `asset link`, custodian `--account` on `account show/disconnect`, `--accounts-url`/`ACCOUNTS_URL`/`config set
  accounts`, `host migrate`; `session`, `--test`, `--visibility org` and `--principal` removed or hidden; discovery
  before any async runtime.
- `mcport-server`: owner-only `GET /api/v1/hosts/{host}/legacy-accounts` for `host migrate` (+1 test).
- Docs: bundled guides (`crates/mcport-cli/README.md`, `crates/mcport-cli/docs/development.md`), crate READMEs,
  `README.md`, `docs/usage.md`, `docs/development.md`, `docs/architecture.md`, `docs/ASSETS.md`; pre-0.3.0 evidence
  moved to `docs/history/`. There is no docs sync script in this repo; a unit test guards the bundled guides instead.

Commits: `2f2fa5a` legacy-accounts endpoint · `0ca4271` client crate and CLI · `9c3fee5` docs · `d877e9b` records ·
`cd43f1e` storage checked before a token is spent, `sign_in_revoked` refreshes once · `b186fc8` `login status` refreshes
once before calling a sign-in ended · (this update).

Tests (all with `CARGO_TARGET_DIR=target/mig`):

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --locked --workspace --no-fail-fast` (at `b186fc8`) | pass: 156 tests + 3 doctests (server 71, daemon 18, mcp 12, cli 11 unit + 15 binary, client 11 unit + 8 sign-in, api 10) |
| `cargo check --workspace --all-targets --locked` on the exported tree of `2f2fa5a` alone | pass |
| `cargo package --list --allow-dirty --offline -p <crate>` for core, api, client, daemon, cli | file lists include the bundled guides |

What the new tests cover: the client against a stub Silicon Accounts (`crates/mcport-client/tests/sign_in.rs`: device
flow pending → slow_down → pending → tokens, denied, expired; short-lived token ok, already used, expired, wrong app,
unknown, sign-in ended, public client off, non-`slt_` input not sent; refresh rotation and a spent token; revoke form;
https-only; session file modes, eight concurrent refreshes rotating once, a passing failure keeping the file, a spent
token deleting it, unreadable/old-format/linked files refused). The real binary against a stub Accounts + backend
(`crates/mcport-cli/tests/cli_accounts.rs`: discovery in an empty home with golden JSON, the hidden alias, exit codes,
nothing written; Silicon `--slt-stdin` with no token in any file; refusals with recovery and no echo; Carbon device
flow with `--json` lines and a replaced sign-in revoked; two concurrent commands refreshing once; a refused token
refreshed and the command repeated; a spent refresh token; an unreachable Accounts while the token still works; logout
revoke form; 0.2 sign-ins ignored and removed; backend/Accounts mismatch refused before spending the token; an
unwritable home refused before spending the token; `sign_in_revoked` confirmed by a refresh and forgotten; `login
status` refreshing once on a refused token; settings precedence). Unit: argument grammar (documented forms parse, removed ones do not), every help page has a description
and none names the removed concepts, the bundled guides.

### Proofs against the shared Accounts stack (real binaries)

Setup: `.mig/cli/stack-up.sh <copy of .mig/accept/server>` started `target/mig/debug/mcport-server` on `127.0.0.1:4241`
(`ACCOUNTS_URL=http://localhost:9590`, `ACCOUNTS_API_URL=http://127.0.0.1:9589`, mcport's dev secret, the S25 webhook
secret, `MCPORT_ALLOWED_UPSTREAM_ORIGINS=http://127.0.0.1:4243`) and `tests/e2e/fixtures.py --port 4243` for MCP
providers. The data directory was a copy of the stage-1 acceptance database (a migrated 0.2.0 database). Every CLI
command ran as `env -i PATH=/usr/bin:/bin HOME=$H SILICON_HOME=$H MCPORT_URL=http://127.0.0.1:4241
ACCOUNTS_URL=http://localhost:9590 target/mig/debug/mcport …` with a fresh `$H` per identity. Run id `1791598634`.

1. **Discovery in an empty `SILICON_HOME`/`HOME`** (`env -i PATH=/usr/bin:/bin HOME=$D SILICON_HOME=$D mcport …`):
   `--help` exit 0 (45 lines); `accounts --json` exit 0 →
   `{"accounts_url":"https://accounts.teamofsilicons.com","api_url":"https://backend.mcport.teamofsilicons.com","app_id":"mcport","backend_url":"https://backend.mcport.teamofsilicons.com","client_id":"mcport","device_flow":true,"docs_url":"https://github.com/teamofsilicons/silicon-mcport/tree/main/docs","install":"silicon-apps install mcport","package_url":"https://crates.io/crates/mcport-client","public_client":true,"repository_url":"https://github.com/teamofsilicons/silicon-mcport","sign_in":{"carbon":"mcport login","silicon":"silicon-accounts login --app mcport -q | mcport login --slt-stdin"},"status":"mcport login status --json","version":"0.3.0","website_url":"https://mcport.teamofsilicons.com"}`;
   `login status --json` exit 0 → `{"authenticated":false}`; `login status` exit 1; `iam --json` exit 0, same object;
   files written: 0. The same three commands under Silicon Apps' macOS runner policy (`/usr/bin/sandbox-exec` with its
   exact deny-default profile, `HOME=SILICON_HOME=TMPDIR=<pkg>/.scratch`, `APPS_TELEMETRY=0`): exit 0, 0, 0 with the
   same output; scratch files: 0.
2. **Silicon:** `mint.mts silicon --custodian-email mcport-cli-c2-1791598634@example.test --handle
   mcport-cli-s2-1791598634` → `si:mcport-cli-s2-1791598634` = `a9X`, custodian `c:mcport-cli-c2-1791598634` = `yQe`;
   `mint.mts slt --app mcport`; `printf %s "$SLT" | mcport login --slt-stdin --json` exit 0 →
   `{"authenticated":true,"custodian":{"id":"c:mcport-cli-c2-1791598634","uuid":"yQe"},"id":"si:mcport-cli-s2-1791598634","kind":"silicon","method":"slt","uuid":"a9X",…}`.
   `login status --json` → same with `"verified":true`; `--offline` → `"verified":false`. The sign-in file is
   `-rw-------` in `drwx------` directories and contains no `slt_`. `connection new notes --transport http --url
   http://127.0.0.1:4243/mcp/public --auth none --json` → `{"access":"owner","id":"0sx","visibility":"invited",…}`;
   `tool ls notes` → `['echo','write','whoami']`; `tool call notes echo --input '{"message":"hello from a Silicon"}'`
   → `{"call_id":"kor","result":{…"isError":false,"structuredContent":{"account":"public",…}}}`; `activity ls` lists
   `kor` with caller `si:mcport-cli-s2-1791598634`.
3. **Carbon (device flow):** `mcport login --json` in the background printed
   `{"event":"device_code",…,"user_code":"JNC4-4FC6","verification_uri":"http://localhost:9590/device",…}`;
   `mint.mts approve --email mcport-cli-c2-1791598634@example.test --code JNC4-4FC6` → 204; the CLI then printed
   `{"authenticated":true,"id":"c:mcport-cli-c2-1791598634","kind":"carbon","method":"device","uuid":"yQe",…}` and
   exited 0. `login status --json` → `"verified":true`. Custodian rule through the CLI: `connection ls` →
   `[('0sx','notes','custodian','si:mcport-cli-s2-1791598634')]`; `activity ls` shows the Silicon's calls; `tool call
   0sx whoami` succeeds as the Carbon (call `cmo`).
4. **Sharing and the allow list:** a second Carbon (`c:mcport-cli-out-1791598634` = `7HL`, device flow) sees `[]`;
   `tool ls 0sx` → 404 `not_found`; `access new mine --account si:mcport-cli-s2-1791598634` → `silicon_not_reachable`
   with recovery `mcport allow add c:mcport-cli-out-1791598634`; the custodian ran `allow add c:mcport-cli-out-… --silicon
   si:mcport-cli-s2-…` (exit 0); the Silicon's `allow ls` shows it; the same `access new` then succeeded and the
   Silicon's `connection ls` shows `('Ukl','mine','invited','c:mcport-cli-out-1791598634')`; `allow rm` → `{"deleted":true}`.
5. **Refusals (real Accounts messages):** the used token → `slt_already_used`; a token minted for `remind` →
   `slt_wrong_app` ("issued for the app 'remind', not for 'mcport'"); `--slt slt_not-a-real-token` → `slt_unknown`
   (human output `Error [slt_unknown]: …` + `Recovery: …`); a token used 149 s after minting → `slt_expired`; each exit
   1, each recovery naming `silicon-accounts login --app mcport -q | mcport login --slt-stdin`, none echoing the token.
6. **Refresh rotation:** with the Carbon's stored `expires_at` forced to now+30 s, `connection ls` refreshed (refresh
   token SHA-256 prefix `c0795586ab5f` → `ee0a521831ac`); forced again → `7c8744d973f8` (the second refresh presented
   the persisted token, otherwise Accounts would have revoked the sign-in); forced again with three concurrent
   `connection ls` → one rotation (`5eb3349a01ba`), all three exit 0; file still `-rw-------`.
7. **Sign-out:** the Silicon's `logout --json` → `{"revoked":true,"signed_out":true,"uuid":"a9X",…}`; `login status
   --json` → `{"authenticated":false}`; `connection ls` → `not_signed_in`; a second `logout` → `signed_out:false`. The
   revoke reached the service as webhook event `01a1239b-8ee0-7042-9b02-223a20deb31b` (delivered), logged "Ignoring a
   sign-out MCPort requested" (`app_revoked`, S5).
8. **Sign-in ended elsewhere:** the second Carbon removed MCPort at Silicon Accounts (`DELETE /v1/me/apps/mcport` with
   its first-party token → 204; the service logged "Revoked an account's sign-ins and pending work"); its next
   `connection ls` → `sign_in_ended` ("…revoked at 2026-10-10T02:22:53.807Z (access_removed); sign in again.") and
   `login status --json` → `{"authenticated":false}`.
9. **Positional form and alias:** `mcport login "$SLT" --json` (as the Silicon runtime calls it) → signed in as `a9X`;
   `mcport iam --json` → `app_id` `mcport`.
10. **Mismatch:** `ACCOUNTS_URL=http://127.0.0.1:9 mcport login --json` → `accounts_mismatch` ("…trusts Silicon Accounts
    at http://localhost:9590, but this CLI signs in at http://127.0.0.1:9.") before any Accounts call.
11. **`host migrate` on a real 0.2.0 registry:** the stage-1 baseline run's v1 registry for host `aXv` (owner key
    `c:owner`, org `tos`) was copied into a fresh home, its `backend_url` pointed at 4241, its shared local connection
    `SVs` at the live fixture, and two personal accounts keyed by old ids added (`si:researcher`, linked to `EVW` at
    cutover, and `si:gone-old`, never linked). Signed in by device flow as `c:mcport-owner-87242` = `2Cd` (the host's
    linked owner). `daemon status` → `registry_version 1` + "Registered before Silicon Accounts: run mcport host migrate
    aXv on this machine."; `account show SVs` → `registry_not_migrated` with the two commands; `host migrate aXv
    --dry-run` → both keys unmapped (the daemon had not reported them); `host migrate aXv` → `unmapped_accounts`
    (exit 1). `daemon start` ran the 0.3.0 daemon on the v1 registry; `tool call SVs echo` through it succeeded (call
    `Mii`). Dry run again → `si:researcher → EVW (si:mcport-scholar-87242)`, `si:gone-old` unmapped; `host migrate aXv
    --drop-unmapped` → `migrated:true`, `registry_version:2`, `backup:registry.v1.json`, `dropped:["si:gone-old"]`,
    daemon restarted. The registry now has `owner_uuid: 2Cd`, no old owner fields, `wNg` personal keys `['EVW']`,
    both files `-rw-------`; `daemon status` → `registry_version 2`; `account show SVs` →
    `{"account":"2Cd","connected":true,…}`; `tool call SVs echo` through the v2 registry succeeded (call `Egf`);
    `daemon stop` stopped it.

After `cd43f1e`/`b186fc8` the final binary was run again on the same stack: Silicon `--slt-stdin` → `login status`
(`verified:true`) → `tool call 0sx echo` (call `6ec`) → `logout` (`revoked:true`) → `{"authenticated":false}`; Carbon
device flow (code `MJZM-RUX7` approved, 204) → `login status` (`verified:true`) → `connection ls` →
`[('0sx','custodian')]` → `logout`; the three discovery commands under the Silicon Apps macOS sandbox again exit 0, 0,
0 with 0 files written.

All sign-ins made here were signed out at the end (`logout` → `revoked:true`), and every process this stage started
was stopped (`.mig/pids/` is empty; nothing listens on 4240-4259).

Left for later stages:
- **e2e harness** (`tests/e2e/run.py`, `serve.py`, `fixtures.py`) still drives the old sign-in; `scripts/check.py`'s
  e2e step fails until it moves to an Accounts fixture or the shared stack (e2e stage). The CLI side it needs now
  exists.
- **Packaging:** `apps.yaml`, `scripts/package-apps.sh`, `release.yml`, musl/zigbuild targets; `honeycomb.yaml` and
  `scripts/package.py` still exist; `scripts/README.md`, `tests/e2e/README.md` and `deploy/` docs still describe the
  old packaging (ship stage).
- **Website:** the Next.js BFF (bearer tokens, download tickets, `/api/v1/me`, allow list, the Silicon token form).
- **Contract:** paste `understanding-proposal.md` (now with the CLI additions) into `UNDERSTANDING.md` (Carbon only).

Blocked on: nothing.

Gotchas:
- The service's connection and activity views show `display_name: ""` for accounts it only knows from tokens and
  lookups (Accounts lookups carry no display name); the CLI's own sign-in shows the name from the token response.
  Worth a look in the website stage.
- `nohup python3 …` on this Mac starts a shim whose pid is not the interpreter's: record the pid listening on the port
  (`lsof -t -iTCP:4243 -sTCP:LISTEN`) or the fixture outlives `kill`.
- Never run `mcport login` here with the default `ACCOUNTS_URL` against a backend that does not answer discovery: it
  would start a device sign-in at production Silicon Accounts. Always set `ACCOUNTS_URL=http://localhost:9590`.
- zsh does not split `$args`; use `${=args}` when looping over command strings.

## 2026-10-10 — Stage 3 (packaging, CI, deployment configuration, docs): done

Everything around the code now says and does Silicon Accounts and Silicon Apps; a release is one tag away and each
production step is a reviewed command in [cutover.md](cutover.md). Nothing was pushed, uploaded, released or deployed.
Decisions P1–P21 are in [decisions.md](decisions.md); contract additions at the end of
[understanding-proposal.md](understanding-proposal.md).

What changed:
- **Packages:** `honeycomb.yaml`, `scripts/package.py` (+ its tests) and `scripts/requirements.txt` deleted.
  `packaging/apps.yaml.in` + `scripts/package-apps.sh` (bash entry point) → `scripts/package_apps.py` (stdlib only) build
  `dist/apps/mcport-<version>-<target>.tar.gz` + `.sha256` per target: version = workspace version; the bytes must be
  an executable for the target (static ELF on Linux, hard-float on ARMv7, thin Mach-O, PE console program); whenever the
  machine can run it (or through `PACKAGE_APPS_EMULATOR`), `--version`, `--help`, `accounts --json` and
  `login status --json` must answer as Silicon Apps' workers require in an empty home with nothing written; then
  `silicon-apps validate` + `pack` (empty Apps home, no session), archive opened and re-checked from the extracted copy;
  never overwrites. `--check-only [--record]`, `--checked-record`, `--require-discovery`, `--allow-dynamic` (dev only).
- **CI:** `release.yml` (tag `v*` = CLI version, or by hand): 8 targets — static musl Linux via `cargo-zigbuild==0.23.4`
  + `ziglang==0.15.2` (x86_64, i686, aarch64 native on ubuntu-24.04-arm, armv7hf checked through qemu-arm) and the
  existing macOS/Windows runners; tests on each runner's host target; `--check-only` per binary; a packaging job with
  `silicon-apps-cli` 0.2.0 that re-checks Linux archives (natively/qemu), packs macOS/Windows on their records, writes
  `SHA256SUMS` and uploads `mcport-silicon-apps-release`. `checks.yml`: no PyYAML, `check.py --skip-e2e`; `check.py`
  also runs the discovery commands against the debug CLI. `backend.yml`: no web job.
- **Deploy:** the backend bundle is the service alone (`package_backend.py`, `install.py` refuses `web/dist`, tests
  updated); `deploy/mcport.service` description; `deploy/vercel.md` rewritten for the Next.js website (kit variable
  names, callback/origin at Silicon Accounts, CSP, download tickets, verification); `deploy/aws/README.md` names Silicon
  Accounts; `deploy/README.md` says the website deploys separately.
- **Docs:** `scripts/README.md` (packages, release workflow, publishing), `docs/development.md` (releases),
  `README.md` (links), `docs/architecture.md` (replay index without testing environments), `tests/e2e/README.md`
  (status: predates 0.3.0, left out of CI). Service wording: a test handle and two comments (P20).
- **Migration records:** `cutover.md` rewritten end to end (order and dependencies, Silicon Accounts calls, mapping,
  bundle, CLI packages and a development release before the day, website environment, runtime env, the cutover in
  seven steps, Silicons still on the Honeycomb CLI, rollback for service/website/CLI, after); 12 commands marked
  `# run at cutover`. The webhook secret is generated before the day and the URL set only once 0.3.0 answers; the
  IAM-era `runtime.env` is copied aside first because the installer's backup holds the edited file.

Commits: `c85982b` packaging + CI · `fbc9633` service-only bundle · `a831b4f` website deployment · `686042a` docs ·
`d94f1eb` service wording · `bc806c5` `--allow-dynamic` and bash in CI · `4b1ea5c` records · `3f54669` runbook
refinements · (this update).

Tests (all with `CARGO_TARGET_DIR=target/mig`):

| Command | Result |
|---|---|
| `python3 scripts/check.py --skip-e2e --skip-web` (fmt, workspace tests, clippy `-D warnings`, script tests, discovery check), at `bc806c5` | pass, exit 0: 156 tests + 3 doctests (server 71, daemon 18, mcp 12, cli 11 + 15, client 11 + 8, api 10), 41 script tests, discovery `passed` on the debug CLI (`--check-only --allow-dynamic … host`) |
| `python3 -m unittest discover -s scripts/tests -p 'test_*.py'` (after the last change) | 41 pass (package_apps 19, backend bundle 19, directory import 3); the 3 package-flow tests ran the real `silicon-apps` 0.2.0 |
| `python3 -m unittest discover -s tests/e2e -p 'test_*.py'` | 4 pass (fixture self-tests; not part of `--skip-e2e` runs) |
| `cd web && npm ci && npm test && npm run build` (the old web, as CI still runs it) | 38 tests pass, build ok (`node_modules`/`dist` removed afterwards) |
| `actionlint` 1.7.12 with shellcheck 0.11.0 on `.github/workflows/*.yml`; `shellcheck scripts/package-apps.sh` | clean |
| PyYAML `safe_load` of the three workflows | parse (jobs: version/native/package; check; server/assemble) |
| `openapi-spec-validator` on `docs/openapi.yaml` | valid, OpenAPI 3.1.0, 36 paths (unchanged) |

Local release builds (`target/mig-apps`, release profile; `.mig/logs/ship-build-release.log`): `aarch64-apple-darwin`
(cargo, 1m57s cold), `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `i686-unknown-linux-musl`,
`armv7-unknown-linux-musleabihf` (cargo zigbuild, ~2 min each; aws-lc-sys builds for all four) and
`x86_64-apple-darwin` (cargo). `file` and the packager agree: four "statically linked" ELF executables (ARM e_flags
`0x5000400`: EABI5, hard-float) and two thin Mach-O executables.

Packaging proofs (real binaries, real `silicon-apps` 0.2.0, empty Apps home):
1. `scripts/package-apps.sh 0.3.0 <target> <binary>` for all six → six archives, each holding exactly `apps.yaml`
   (only its target) and `bin/mcport` (0755); `.sha256` files and a `SHA256SUMS` verify (`shasum -a 256 -c`). The
   macOS arm64 run: discovery `passed here` and `archive_checked: true`; the others: "not run: this machine (Darwin arm64)
   cannot run a <target> binary" (this Mac has no Rosetta, Docker or qemu-user), with the warning to check on a matching
   machine — CI does that. Packing twice gave byte-identical archives (macOS arm64 `f6fe1f97d588…`, linux-x86_64
   `6accd0a6e132…`).
2. Each archive extracted into an empty directory → `silicon-apps validate` → `valid` with its single target.
3. From the extracted macOS archive, in an empty `HOME`/`SILICON_HOME` with `env -i`: `--help` exit 0 (3153 bytes),
   `accounts --json` exit 0 (`"app_id":"mcport"`, `"version":"0.3.0"`, `"install":"silicon-apps install mcport"`),
   `login status --json` exit 0 `{"authenticated":false}`, 0 files written. The same three under Silicon Apps' macOS
   worker policy (`sandbox-exec` with `runner/server.py`'s exact profile): exit 0, 0, 0; 0 scratch files.
4. Refusals: version `0.3.1` ("does not match the workspace version 0.3.0"); a Mach-O named `linux-x86_64` ("not a Linux
   (ELF) executable"); arm64 Mach-O named `macos-x86_64` ("CPU type 0x100000c, not x86_64"); `--require-discovery` on a
   Linux binary here ("no --checked-record was given"); `--check-only` of the x86_64 Mach-O here ("cannot run a
   macos-x86_64 binary"); a second packaging of the same target ("never overwritten").
5. Production Silicon Apps, read anonymously with an empty home (P19): `silicon-apps show mcport` → 404 `not_found` (no
   public listing yet); `GET /v1/capabilities` → live workers `linux-x86_64`, `linux-i686`, `linux-aarch64`,
   `linux-armv7hf`; Windows and macOS `not_configured`.

Sweep (`git grep -n -i -E 'iam|honeycomb|org_id|organi[sz]ation|\borg\b|tenant'`), every remaining hit intentional:
- `web/` (61 hits, 9 files): the old Vite website, replaced wholesale by the web stages.
- `tests/e2e/` (fixtures.py, run.py, serve.py, test_fixtures.py, directory_journey.py, README.md): the pre-0.3.0 journey
  and its fake identity service; left for the end-to-end stage to move to Silicon Accounts fixtures (P12), flagged in
  its README.
- `docs/history/` (60 hits) and `docs/migration/`: historical records and this migration's notes.
- `understanding/UNDERSTANDING.md`: Carbon-only; replacement text is in `understanding-proposal.md`.
- `crates/mcport-server/src/identity.rs`, `identity_store.rs`, `store.rs`, `main.rs`: the `link-identities` mapping
  (`iam_principal_id,accounts_uuid`, per the brief), the `identity_links` table, the legacy `org_id` column and
  `tenant_records` index, the 410 answers for `/api/v1/iam` and `/webhooks/iam`; `state.rs`: the IAM-era variables it
  ignores with a warning; `hosts.rs`, `mcport-core`, `mcport-daemon`, `mcport-client/src/local.rs`: the `org_id`
  transition fields and registry v1 migration for daemons released before 0.3.0; `connections.rs`, `directory.rs`,
  `operations.rs`: legacy-record comments and tests; `auth.rs`: tests of the removed routes; `oauth.rs`: `/tenant` is a
  provider's multi-tenant issuer path in OAuth discovery tests (generic OAuth, not an account grouping).
- `crates/mcport-cli`: the hidden `iam --json` alias (brief: one minor release), tests that help and guides never name
  the removed concepts, legacy sign-in fixtures, the `--visibility org` refusal.
- `crates/mcport-server/catalog/community.json`: third-party MCP descriptions ("content organization", "tenants").
- `deploy/aws/production.json`, `deploy/aws/README.md` (`CAPABILITY_IAM`): AWS IAM, not the Silicon identity service;
  `deploy/aws/deploy.md`: the old variables to remove at cutover.
- `docs/API.md`, `docs/openapi.yaml`: the documented 410s and transition fields.

Left for later stages:
- **End-to-end stage:** move `tests/e2e` (fixtures.py's identity fake → a Silicon Accounts fake: JWKS, device flow,
  public-client SLT exchange, refresh/revoke, lookups, signed webhooks; run.py's journey → the 0.3.0 CLI), then drop
  `--skip-e2e` from `checks.yml`. Scenario 7 can use `scripts/package-apps.sh` (macOS arm64 runs the commands here).
- **Web stages:** replace `web/` with the Next.js kit; add the web job to `checks.yml` (pnpm) in place of check.py's npm
  steps; keep `deploy/vercel.md` accurate if a variable name changes; the website's Silicon short-lived-token form.
- **CI on GitHub:** the release workflow has not run (nothing may be pushed); the first run proves the Linux discovery
  checks (natively and under qemu) and the Windows path handling.
- **Contract:** paste `understanding-proposal.md` into `UNDERSTANDING.md` (Carbon only).

Blocked on: nothing. (Linux binaries cannot run on this Mac — no Docker, no qemu-user, no Rosetta — so their discovery
checks run in CI.)

Gotchas:
- Bash calls start in the session's primary directory (another worktree with its own `rust-toolchain.toml`): always
  `cd` into this worktree first, or `rustup`/`cargo` act on the wrong toolchain (P21).
- This worktree uses the `stable` toolchain (1.98.0); release targets must be installed there.
- zigbuild needs `CARGO_ZIGBUILD_ZIG_PATH=$HOME/.local/share/uv/tools/ziglang/lib/python3.14/site-packages/ziglang/zig`
  and `$HOME/.local/bin` on `PATH` (cargo-zigbuild) on this Mac.
- `silicon-apps` here is signed in to production: the packager strips its session variables and gives it an empty home;
  never call it with the default home for anything but `--version`.
- `dist/apps/` (gitignored) holds this stage's six local archives; `target/mig-apps` holds the release builds.

## 2026-10-10 — Stage 4 (end to end against Silicon Accounts): done

The service, CLI and host daemon were run end to end with real Silicon Accounts tokens on the shared local stack, in
eight scripted scenarios; two defects they exposed were fixed in the service; the real-binary regression journey moved
to a fake Silicon Accounts and runs in CI again. Decisions E1–E15 are in [decisions.md](decisions.md); cutover step 9
and a note in step 7 were added to [cutover.md](cutover.md); contract notes at the end of
[understanding-proposal.md](understanding-proposal.md).

What changed:
- `scripts/dev-accounts.sh` / `dev-accounts-stop.sh` → `scripts/dev_accounts.py up|down|restart|status`: MCP fixtures
  on 127.0.0.1:4242, `mcport-server` on 127.0.0.1:4241 (SQLite data in `.local/dev-accounts/server`; no Postgres, E1)
  against a loopback Silicon Accounts; webhook registered with mcport's app credentials, secret kept 0600 outside git,
  a test ping must be delivered (a stale secret is replaced and the service restarted); idempotent; pids per
  `MCPORT_DEV_PIDS` (here `.mig/pids`).
- `scripts/e2e-accounts.sh` → `tests/e2e/accounts_stack.py` (eight scenarios, 125 checks) with
  `tests/e2e/stack/mint.mts` (identities through the stack's testkit).
- Service fixes: names and photos from the user base (E10, `d8ad585`); same-second revocation ties settled by
  introspection, deferred work and tickets treat them as later (E11, `c14424c`). +2 unit tests; the stub's lookups now
  answer like Silicon Accounts.
- `tests/e2e`: `accounts_fake.py` + `ed25519.py` (+ `test_accounts_fake.py`), `run.py` and `directory_journey.py`
  ported (102 checks), `fixtures.py` without the old identity fake, `serve.py` on the same stack, README rewritten;
  `checks.yml` runs the journey again and keeps its logs on failure; `scripts/README.md`, `docs/development.md`,
  `deploy/testing.md` updated.

Commits: `d8ad585` names and photos · `c14424c` same-second revocations · `010a4d4` dev stack and stack scenarios ·
`c91950e` journey on a fake Silicon Accounts, CI · `3968011` stale-secret recovery · (this record).

Tests (all with `CARGO_TARGET_DIR=target/mig`):

| Command | Result |
|---|---|
| `MCPORT_E2E_BASE=4250 python3 scripts/check.py --skip-web --e2e-run-dir …` (after the Rust changes) | exit 0: fmt; 161 Rust tests incl. doctests (server 73); clippy `-D warnings`; 41 script tests; discovery `passed`; 7 e2e fixture tests; journey 102 checks |
| `cargo test --locked -p mcport-server` (after each fix) | 72, then 73 pass |
| `python3 -m unittest discover -s scripts/tests -p 'test_*.py'` (HEAD) | 41 pass |
| `python3 -m unittest discover -s tests/e2e -p 'test_*.py'` (HEAD; `python3 -S`: the `cryptography` cross-check skips) | 7 pass |
| `python3 -I tests/e2e/run.py --no-build --base 4250` (HEAD) | 102 checks pass in 125 s; nothing left listening on 4250–4253 |
| `scripts/e2e-accounts.sh` (shared stack, run `1791605986`) | 125 checks pass, exit 0 |
| `scripts/dev-accounts.sh` twice, then after rotating the secret at the stack | started → reused (`started: false/false`, ping delivered) → `HTTP 401 … invalid_webhook_signature`, new secret, service restarted, ping delivered |
| PyYAML `safe_load` of `checks.yml` | parses; steps as intended (actionlint is not installed here) |

### The eight scenarios, real outputs (trimmed), run `1791605986`

Identities: `c:mcport-e2e-c1-1791605986` = `Qfj` (Carbon), `si:mcport-e2e-s1-1791605986` = `sy2` and
`si:mcport-e2e-s2-1791605986` = `jwd` in its care, `c:mcport-e2e-c2-1791605986` = `VPW` (unrelated Carbon).

1. **Carbon on the API** (hosted sign-in, `--redirect http://localhost:4240/auth/callback --exchange`): `GET /api/v1/me`
   → `{"uuid":"Qfj","id":"c:mcport-e2e-c1-1791605986","kind":"carbon","display_name":"Mcport E2e C1 1791605986","pfp_url":"http://127.0.0.1:9594/pfp/carbon?id=Qfj","custodian":null,…}`;
   no token → `401 authentication_required`; broken signature → 401; `POST /api/v1/connections` → `M3e`
   (`access: owner`, `visibility: invited`); list, read, `PATCH` (live) with version, stale version → 409; a tool call
   through `/mcp` recorded with caller `Qfj`, status `completed`; `DELETE` → `{deleted:true}`, then 404 `not_found`.
2. **Silicon on the CLI**: `printf %s "$SLT" | mcport login --slt-stdin --json` →
   `{"authenticated":true,"kind":"silicon","uuid":"sy2","custodian":{"uuid":"Qfj",…},"method":"slt",…}`; the sign-in
   file is 0600 and holds no `slt_`; `login status --json` → `"verified":true`; `connection new s1-notes` (`ARc`),
   `connection ls`, `tool ls` (echo, write, whoami), `tool call … echo` → `"hello from a Silicon"` (call `zpa`),
   `activity ls`/`show`, `connection set --description`; `asset ls`/`get` (0600 PNG) and `asset link` (downloads once
   without a token, then 404 `download_expired`); forced expiry → one refresh (refresh token rotated), three concurrent
   commands → all exit 0 and the sign-in survives; `access new … --account c:mcport-e2e-nobody-…` → 404
   `unknown_account`; `logout --json` → `{"revoked":true,"signed_out":true,"uuid":"sy2",…}`; `login status --json` →
   `{"authenticated":false}`.
3. **Device flow**: `mcport login --json` printed the code and `http://localhost:9590/device`; `mint.mts approve` → 204;
   the CLI ended `{"authenticated":true,"kind":"carbon","method":"device","uuid":"Qfj",…}`; `connection ls` shows `ARc`
   with `access: custodian` and owner `display_name: "mcport-e2e-s1-1791605986"` plus photo (E10). A second code denied
   at Silicon Accounts (`POST /v1/device/SBTX-AP7V/deny` → 204) → `{"error":{"code":"device_denied",…}}`, exit 1,
   still signed out.
4. **Custodian, circle and sharing**: the Silicon signed in again with `silicon-accounts login --app mcport -q | mcport
   login --slt-stdin`; the custodian manages `ARc` (visibility → `circle`, switched `echo` off → the Silicon's call
   `tool_disabled`, on again), sees the Silicon's activity and result, and its own call there is recorded as its own;
   the second Silicon (created with the Carbon's first-party token) sees `ARc` as `circle` and uses it; the unrelated
   Carbon sees nothing, `tool ls ARc` → 404 `not_found`; shared by `c:` id → `invited`, it calls (the owner does not
   see that call); unshared → 404 again and its earlier result is hidden; sharing by uuid works and shows the id;
   `access new c2-tools --account si:…` →
   `{"code":"silicon_not_reachable",…,"recovery":"Ask si:mcport-e2e-s1-1791605986 or its custodian to allow c:mcport-e2e-c2-1791605986 (mcport allow add c:mcport-e2e-c2-1791605986), then share again."}`;
   the custodian's `allow add … --silicon si:…` → allowed, share reaches the Silicon; the Silicon's `allow rm` → new
   shares refused, the old one stays; a Carbon is reachable without an allowance; a Carbon's circle connection reaches
   its Silicons, not other Carbons. Provider accounts: per-user `c1-bearer` → `provider_authentication_required` until
   the Silicon connects its own; the custodian inspects it (no secret) and disconnects it; the unrelated Carbon gets 404.
   Directory: the Silicon's personal entry is managed by its custodian, hidden from others until shared by id, used to
   make a connection, hidden again when unshared. Host: `host new` + a stdio MCP (`FIXTURE_ACCOUNT=stdio-e2e`) →
   ready; the Silicon's call answers `stdio-e2e`; `daemon status` → `registry_version: 2`; `daemon stop` → the call
   answers `host_offline`; `daemon start` → works; `host rm` → deleted. Settings are per account (telemetry off for the
   Carbon only); a report with no mail service → `status: delivery_failed` with an id.
5. **Webhooks**: the custodian's `POST /v1/me/silicons/sy2/id` (twice) → the service showed each new id; the Silicon's
   own `login status` shows it, same uuid; Silicon Accounts' replay of the first `account.id_changed` (same `event_id`)
   → delivered with HTTP 200 and the newest id stayed; a self-signed `ping` → `{"received":true}`, again →
   `{"duplicate":true,"received":true}`; another secret → `401 invalid_webhook_signature`; a 15-minute-old timestamp
   or a changed body → 401. `silicon-accounts --json apps remove mcport` (the Silicon's first-party sign-in) → its
   previous access token → `401 signed_out`, live route 401, CLI → `sign_in_ended` ("…revoked at …
   (access_removed); sign in again."); meanwhile the custodian still sees `ARc` and the other Silicon does not; a new
   sign-in restores both. `mcport logout` (Silicon 2): the old token still reads (local JWT, `app_revoked` ignored) but
   the live route answers `sign_in_revoked`. Custodian rename → `account.updated` shows the new display name. STK
   rotation → older token `signed_out`, CLI `sign_in_ended`, new STK signs in. Transfer to the unrelated Carbon
   (accepted with its first-party token) → it manages `EMS` as custodian, the old custodian loses it at once, the
   Silicon no longer sees the old circle. `DELETE /v1/me/silicons/jwd` → its token `account_deleted`, `EMS` gone.
   `DELETE /v1/me` (unrelated Carbon) → the connections it had shared disappear for the Silicon and the Carbon it shared
   with. A refresh token redeemed elsewhere → the CLI's own refresh is a reuse: `sign_in_ended` ("This refresh token was
   already used once…"), the copy's access token → `signed_out`, its refresh token `invalid_grant`.
6. **Proofs**: `Authorization: Proof sap_not-a-real-proof` and a real User verification proof issued by `interface`
   for `mcport` (scope `mcport.connections.read`) → both `401 proof_not_accepted` (E7); the proof was revoked.
7. **Discovery from a packed archive**: `scripts/package-apps.sh 0.3.0 macos-aarch64 target/mig/debug/mcport
   --output-dir <run>/package` → `{"archive_checked":true,"discovery":"passed here","silicon_apps":"0.2.0",…}`; the
   archive holds `apps.yaml` (this target only) and `bin/mcport`; extracted, in an empty home: `--help`, `accounts
   --json` (`"app_id":"mcport"`, `"version":"0.3.0"`) and `login status --json` (`{"authenticated":false}`) exit 0;
   nothing written.
8. **Restart**: `dev_accounts.py restart` → `{"restarted": true, "service": 32326}`; the Carbon's and the Silicon's
   stored sign-ins work without signing in again; scenario 1's API token is still accepted; the Silicon's token from
   before it removed MCPort is still refused (`signed_out`, persisted); the earlier `ping` event id still answers
   `{"duplicate":true,"received":true}`; a new Silicon Accounts test ping → delivered.

The defects: run `1791603267`'s transcript showed every `owner`, `caller` and `account` with `"display_name":""` and
no photo although the stack's user base had both (`GET /v1/apps/mcport/users/{uuid}`); the scenario 1 and 3 checks added
for it pass since E10. Run `1791603867` failed scenario 5 ("the old token to be refused after the STK rotation: not
within 25 s") because the Silicon signed in and its STK was rotated within the same second; it passes since E11.

Left for later stages:
- **Web stages:** the Next.js website and its BFF; the website's own end-to-end checks (sign-in through the hosted
  pages, the Silicon token form, download tickets) can reuse `scripts/dev-accounts.sh` (website slot 4240).
- **CI on GitHub:** the journey has run on macOS here; its first Linux run happens on the first push (stdio and host
  daemon paths are the same code).
- **Contract:** paste `understanding-proposal.md` into `UNDERSTANDING.md` (Carbon only).

Blocked on: nothing.

Gotchas:
- mcport's webhook secret on the shared stack now lives in `.local/dev-accounts/webhook-secret`; S25's
  `.mig/accept/webhook-secret` is stale. `dev-accounts.sh` heals a stale secret by itself.
- Two `membership.signed_out` deliveries from 02:23 (the CLI stage, while no service listened) were still retrying at
  the stack; they reach whatever runs on 4241 later and are harmless.
- Scenarios build on each other: `--only 4` alone fails; use `--only 1,2,3,4`.
- serde_json sorts object keys: compare webhook answers as JSON, not text (`{"duplicate":true,"received":true}`).
- Every process this stage started was stopped at the end (`.mig/pids/` empty; nothing listens on 4240–4259).

## 2026-10-10 — Resumed backend review fixes

Fixed the recovered review's late-profile replay: profile versions no longer overwrite an id or custodian learned from a newer event. Added a transfer regression that delivers a high-version older profile. Identity linking now refuses distinct legacy public identities targeting one account and mismatched Carbon/Silicon kinds, without changing records. New hosts cannot query legacy identity mappings; only hosts preserved by the migration can use that endpoint. Connections whose owner removed app access cannot execute MCP calls, including calls from a custodian. Deletion removes grants and allowances the deleted account created as well as those naming it. Failed age-based JWKS refreshes now share the unknown-key backoff.

Validation: `cargo test --locked -p mcport-server` (75 passed), `cargo clippy --locked -p mcport-server --all-targets -- -D warnings`, `cargo fmt --all` (2 Rust build jobs, debug and incremental off).

Still outstanding: daily reconciliation of deletion when no webhook arrives, out-of-order access-removal handling, legacy production-version cutover documentation, additional regression coverage for freeze/deleted-sharer behavior, actual shared Accounts E2E rerun, and the complete Next.js frontend. This section records a checkpoint, not completion of the migration.

## Account reconciliation review — 10 October 2026

Account lookups now carry their start timestamp and cannot overwrite a newer webhook or a deletion tombstone. Custodian/circle checks fail closed when a Silicon cache exceeds its freshness bound and Accounts cannot confirm it. The background worker checks up to 50 retained accounts per minute when their last lookup exceeds one day; a confirmed deleted status purges owned data and grants, while an ambiguous not-found retains data. Accepted token issue times are persisted in additive SQLite schema version 2 so an old access-removal delivery cannot freeze an account that already signed in again. Deletion clears cached custodian fields. Operator inventory and linking require an existing store and master key before any initialization, avoiding silent empty stores from a mistyped path.

Tests: server suite 78/78 passed before the final operator guard; added delayed-lookup, expired-custodian, deleted-account sweep and late-access-removal regressions. Final lint/testing follows.

## Next.js website completion — 10 October 2026

The full product now runs on Next.js 16/React 19 with Accounts/Apps styling and Arc controls. Product pages cover connection setup and versioned configuration, tool discovery/policy/execution/structured files, provider credentials/OAuth, exact-account grants, shared directory entries, resource templates/resources/prompts, activity/cancellation, host removal, custodial provider inspection, inbound sharing allowances, telemetry and reports. The website uses sealed server sessions and a same-origin backend proxy; one-use asset tickets bypass platform body limits. Existing Vite runtime code is removed; original checkout remains untouched. CI/check.py now use pinned pnpm, typecheck, lint, unit tests and the Next production build.

Validation: 44 web unit tests, **29 real hosted Accounts + product browser checks** (`.mig/web-final.log`), including WCAG 2.2 AA light/dark desktop/phone, refresh concurrency and signout. Actual product browser flows create a connection against the local MCP provider, discover seven tools, execute a structured call, persist permissions/grants, read resources/prompts, create/share a directory entry and check mobile pages for overflow/client errors. Screenshots are in `.mig/screens/mcport-*.png`. Typecheck, lint and standalone production build pass.

Live backend/CLI Accounts run **125 passed**: `.local/dev-accounts/e2e/run-1791618846/result.json`; covers daemon-based local stdio, authenticated provider accounts, scoped structured assets, account webhooks and restarts. Controlled providers do not establish compatibility with every external provider. No production release or push occurred.

## 2026-10-10 — Real provider proof and 128-bit Accounts backfill

The official `@modelcontextprotocol/server-filesystem@2026.8.31` ran as an actual stdio MCP through a registered local daemon. An unrelated invited Carbon read and wrote the configured allowed folder; `/etc/hosts` was refused, then revoking its share removed access. Evidence: `.mig/official-proof.log`, `.mig/official-evidence/transcript.json`. This is in addition to the 125-check live Accounts/CLI/API suite and the 29 browser journeys recorded above. Credentials stayed local.

Canonical lowercase 36-character account UUIDs and case-sensitive legacy keys are accepted during transition. SQL schema step 3 adds an immutable mapping ledger. `mcport-server migrate-account-uuids --file export.csv` previews in a rolled-back transaction; adding `--apply` commits the same export. It re-seals declared identity columns and values using the original record environment/AAD, re-keys grants, tool policies, credentials, provider epochs, settings and directory shares, updates custody/allowances/legacy links, and preserves public IDs, provider token values, call results, user payloads and legacy audit snapshots. Old signed subjects are rejected by the ledger, never translated for authentication. Execution replay hashes retain their old namespace through a separate data-only lookup so retries cannot repeat actions across cutover. In-progress provider OAuth attempts expire; established provider credentials remain valid. Queued/running calls must be drained first.

`mcport migrate-account-uuids --file export.csv` handles every stopped daemon registry for this home's selected backend. It keeps personal provider credentials, host tokens, endpoints and journal bytes, re-keys owner/account references, and atomically records immutable replay history. A running daemon, conflicting history or a target collision is refused. `--apply` also removes this home's affected Accounts sign-in; it must sign in again after Accounts invalidates token families. There are no persisted account-bound Briefcase proofs in MCPort. Download tickets and introspection caches live only in memory and disappear when the offline service restarts. Browser session cookies hold revoked tokens and cannot authenticate after Accounts cutover; returning users sign in again.

Regression fixtures cover dry-run/apply/replay/collision rollback, retained encrypted secrets and artifact bytes, inert-environment AAD, custody and shares, provider epochs, directory grants, idempotent execution replay, old-subject 401/new-UUID 200, local credential preservation, unchanged journal bytes and running-daemon refusal (`.mig/uuid-workspace-tests.log`). No production cutover or publication has run.

Final checks: full workspace **178 tests + 3 doctests passed** (`.mig/uuid-final-tests.log`), workspace/all-target clippy passed (`.mig/uuid-final-clippy.log`), and the additional guard against replaying pre-UUID legacy imports passed (`.mig/uuid-final-regression.log`). The actual operator binary also passed against a SQLite backup of the populated local service: **50 mapped accounts, 722 records, 214 records re-sealed, all 690 public records retained**, dry-run unchanged, apply successful and replay zero changes (`.mig/uuid-binary-proof.log`). Every encrypted row was decrypted again with its new AAD; call result and parameter bytes and the private master key were compared unchanged. The running service's original database was not modified by that proof.

### Final preflight and signed delivery fences

The server and stopped daemon registry now reject a partial export that omits any stored legacy account identity. Already canonical accounts and permanent old-subject tombstones are accepted. Signed inbound events carrying a retired subject or custodian are recorded as ignored, so delayed events cannot restore old cache identities or former custody. Provider credentials, host tokens and execution journals remain unchanged. Full workspace tests pass (`.mig/uuid-final-fence-tests.log`), including partial server/registry maps and actual signed stale-custody deliveries, and workspace/all-target clippy passes (`.mig/uuid-final-fence-clippy.log`).

## Coordinated local UUID cutover — verified

The provenance audit identified an older Accounts namespace in the original local fixture: 50 cached identities included 12 current matches, 16 kind collisions, 12 same-kind public-handle mismatches, six empty deleted handles and four missing identities (two deleted, two active). The entire store/master key and original registry homes remain preserved offline, without guessed mappings or partial deletion. These unrelated fixture identities are not claimed migrated.

Created a separate current-namespace store using two existing real Accounts identities, real CLI device sign-ins, an official filesystem MCP server, invited reads/writes, a retained local daemon registry, and a controlled bearer-authenticated provider with encrypted credentials and structured results. Consumed the same final 211-row map as Accounts, SHA256 `750423f3117e11f5eb42025b457ff0f1bf42bd463c31b7e106d9cd10978ca4d9`. Dry-run/apply/replay passed; ten encrypted records were resealed for the new owner AAD. Master key and host credentials were preserved; replay left the logical SQLite dump unchanged. Both CLI sessions were expired and the host registry rekeyed without changing its host ID or credentials.

After restart, both old JWTs returned 401, both fresh device sign-ins used the mapped UUIDs, and the existing daemon reconnected. Invited official filesystem reads retained the original bytes; both historical calls remained readable; the shared bearer provider still returned its structured account result. Sanitized evidence: [uuid-cutover.json](evidence/uuid-cutover.json); detailed local evidence `.mig/cutover-verified.json` and `.migration/cutover/waveform-mcport-apps-consume.json`.

Repacked the current native CLI, including its new registry migration command: `.mig/candidate-uuid/mcport-0.3.0-macos-aarch64.tar.gz`, SHA256 `2b4d290022f64d760008c1db4c0df3641a95c926949fcb336c8c6d79fffe9032`. Empty-home Apps discovery passed. No production changes, publishing or pushing. Final shutdown/restart inventory is in shared `.migration/cutover/services-waveform-mcport-apps.md`.
