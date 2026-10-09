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
| `cargo test --locked --workspace --no-fail-fast` | pass: 126 tests + 2 doctests (server 69, daemon 17, mcp 12, cli 12, client 9, api 7) |
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
`connections::…exact_ids_win…` (now in `access_tests`). New: connection access (6), host custodian/transition (2),
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
