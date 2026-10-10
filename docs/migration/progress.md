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

Commits: `2f2fa5a` legacy-accounts endpoint · `0ca4271` client crate and CLI · `9c3fee5` docs · (this record).

Tests (all with `CARGO_TARGET_DIR=target/mig`):

| Command | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --locked --workspace --no-fail-fast` | pass: 153 tests + 3 doctests (server 71, daemon 18, mcp 12, cli 11 unit + 12 binary, client 11 unit + 8 sign-in, api 10) |
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
revoke form; 0.2 sign-ins ignored and removed; backend/Accounts mismatch refused before spending the token; settings
precedence). Unit: argument grammar (documented forms parse, removed ones do not), every help page has a description
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
