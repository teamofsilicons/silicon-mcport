# MCPort migration decisions

Judgement calls made while moving MCPort from Silicon IAM and Honeycomb to Silicon Accounts and Silicon Apps
(branch `migrate/accounts-apps-20261010`). The Carbon asked for this work and was asleep; nobody could be asked, so each
call below was made, recorded and implemented. They follow the migration brief (D1–D9) and the app decisions file;
where those left room, the reasoning is given. Later stages append their own sections.

## Stage 1 — service (2026-10-10)

### Identity and authentication

| # | Decision | Why |
|---|---|---|
| S1 | Every `/api/v1` route except `/discovery` takes `Authorization: Bearer <Silicon Accounts access token>` (`aud` = `mcport`, `iss` = `ACCOUNTS_URL`), verified locally against the Accounts JWKS. Implemented as axum extractors (`Auth`, and `Live` for introspected routes), so no handler can forget it. | D1/D6. Extractors make authentication structural. |
| S2 | JWKS cached for an hour; an unknown `kid` refetches at most every 30 s (the first fetch and age refreshes don't count); a failed refetch keeps the cached keys. | Key rotation is picked up on the first token signed with a new key, while made-up kids cannot flood Accounts. |
| S3 | Introspection (cached 30 s per token) on: tool execution (`POST …/mcp`), provider account read/connect/disconnect/authorize, sharing (connection and directory grants, allow lists), connection update/delete, tool policies, host creation and deletion. | The union of the decisions file's two lists ("sharing/visibility changes, credential reads, connection deletion" and "tool execution, provider account connect/disconnect/authorize, sharing mutations, host creation"). Reads stay local. |
| S4 | Revocation is per account: `revoked_before` (Unix seconds). A token with `iat` < `revoked_before` is refused (`signed_out`). A token issued at or after it is a new sign-in and reactivates an account whose access was removed. Deleted accounts are refused forever. | Brief "Sign-out signals". Same-second ties go to the new token so a Silicon that rotates its STK and signs in again within a second is not locked out; introspection covers the sensitive routes. |
| S5 | `membership.signed_out` with reason `app_revoked` is ignored; every other reason, `membership.access_removed` and `account.deleted` set `revoked_before`, cancel the account's pending calls and drop its provider OAuth attempts. | Brief. `app_revoked` is one machine's `mcport logout` or the website's sign-out. |
| S6 | Deferred work (host jobs, provider OAuth start/callback, completion) is re-checked with the account row: the account must still be active and not revoked after the work was accepted, plus the usual connection access, tool policy and provider account checks. | Replaces IAM family re-authorization. Webhook-fed; tokens live 30 minutes. |
| S7 | `Authorization: Proof sap_…` is refused with `proof_not_accepted`. Scopes `mcport.connections.read` and `mcport.tools.call` are reserved. | Decisions file: no app calls MCPort today (the survey found no Interface or cross-app calls), so no proof scopes are honoured and `MCPORT_PROOF_ISSUERS` is not added. |
| S8 | The account cache (`accounts` table, plaintext) is filled from token claims, `GET /v1/accounts/{uuid}` lookups and webhooks. A lookup runs on first sight, on a new token family (a new sign-in) and when the row is older than an hour; custodian data that grants someone access is re-checked when older than 10 minutes. Optional lookups stop at 450/minute (Accounts allows 600). Lookup failures keep the cached row. | The circle must be derived live from custodian data without exceeding the lookup limit; webhooks are the primary update path, lookups the backstop. |
| S9 | Caller-supplied `c:`/`si:` ids (and uuids) are resolved through Accounts on every use (cached 60 s), never through the local cache; unknown or deleted ids answer 404 `unknown_account` with the exact id. | Ids can be reassigned 10 days after release; nothing is matched by id after resolution. |
| S10 | Webhook events change a row only when they are newer than what MCPort applied (`occurred_at` vs `synced_at_ms`; `account.version` for profile updates). Events about accounts MCPort never saw create rows only for revocations (so stale tokens stay refused). | Delivery order is not guaranteed. |
| S11 | The service refuses to start without `MCPORT_APP_SECRET`, with an `http://` Accounts URL for a non-loopback host, or with a `MCPORT_ACCOUNTS_WEBHOOK_SECRET` that is not `whsec_…`; each message names the variable. A missing webhook secret is a loud warning (and the endpoint answers 503), not a refusal. IAM-era variables are ignored with a warning naming each. | Brief step 3. The webhook is not needed to serve, but production readiness requires it (deploy docs say so). |
| S12 | `ACCOUNTS_URL` is the public URL and token issuer; optional `ACCOUNTS_API_URL` is where MCPort calls Accounts (the local stack splits them: `localhost:9590` vs `127.0.0.1:9589`). `ACCOUNTS_ISSUER` (named in the survey) is not used. | Stage instructions. |

### No organizations: old concept → new behaviour

| Before (IAM era) | After (0.3.0) |
|---|---|
| Every request bound to principal + org; sessions per org; `--org` switching | One account per token; no org anywhere in the API or storage keys. |
| Connection visibility `org` (everyone in the owner's org) | `circle`: the owner's circle (a Carbon and the Silicons it looks after; a Silicon, its custodian and the custodian's other Silicons). API value `circle`; user copy never says "circle". Requests with `org` get 400 `visibility_removed`. Stored `org` rows read as `circle`. |
| Visibility `invited` / legacy `private` | `invited` (default for every new connection, also for auth `none`: the old "org by default" for no-auth connections is dropped). `private` is accepted as `invited`; updating to `private` atomically removes grants. Grants now count for either visibility. |
| Owner manages a connection | Owner, or the custodian of a Silicon owner (edit, share, policies, shared provider account, delete). A custodian may also use it; its calls are recorded as its own (never as the Silicon). |
| Invitations to members of the same org (`principal_id`) | Grants to any account by `c:`/`si:` id (resolved to uuid, shown by current id, `created_by` recorded). `principal_id` is accepted as an input alias. |
| Silicons reachable by anyone in the org | A Silicon outside the sharer's circle can be shared with only after the Silicon or its custodian allowed the sharer: new `GET/POST /api/v1/allow`, `DELETE /api/v1/allow/{account}` (the CLI stage adds `mcport allow add|rm|ls`, mirroring DM). Carbons are reachable by anyone signed in. Existing grants are kept. |
| Per-principal tool policies | Per-account (uuid); input `account` (id or uuid), output an `AccountRef`. |
| Provider credentials keyed by (connection, owner, org) | Keyed by (connection, account uuid). The custodian of a Silicon can see and disconnect (`?account=si:…`) but never create the Silicon's personal provider account. |
| Name uniqueness per org | Per owner (the `org_id` storage column now holds the owner's uuid as the name namespace). Resolution: exact id, then the caller's own, then any usable; ambiguity → 409 with ids and owners. |
| Hosts per owner + org; daemon rejects other orgs | Hosts belong to the owner; the custodian of a Silicon owner may list, show and delete them; only the owner adds connections to a host. The daemon-side org check is satisfied by the transition fields (S20). |
| Activity visible to the caller in its org | Visible to the caller and the caller's custodian (including cancel and result downloads). Connection owners still do not see other accounts' calls. Revoking the caller's access hides its old results from both. |
| Directory `org` entries (whole org; 1000 per org) | Personal entries: creator + creator's custodian + explicit shares (new `…/directory/{id}/access`, Silicons-not-open rule applies); 1000 per account; `org` source reads as `personal`. The community catalog stays readable by everyone signed in. |
| Settings per (environment, org, principal) | Per account. |
| Reports show org and reporter | Reporter's current id and uuid. |
| Telemetry carries environment, org, actor | `account_uuid` and `account_kind`; the `production` spool path is kept so queued events survive; per-test keys removed. |
| Org membership revocation (live introspection on every request) | S3–S6. |
| Roles, tags, trust, reports_to | Never used by MCPort; nothing to map. |
| Account removed or deleted | Removed access: tokens refused, pending work cancelled, everything the account owns or shares is frozen (hidden from invitees and circle, no execution); its custodian can still view and delete. Deleted: per the decisions file — its connections (with their grants, policies, provider accounts and attempts), hosts, directory entries, calls, settings, reports, personal provider accounts, allowances and identity links are deleted; its grants and policies on others' connections are removed; others' data stays. |

### Routes and wire format

| # | Decision |
|---|---|
| S13 | New: `GET /api/v1/discovery` (public; replaces `/api/v1/iam`), `GET /api/v1/me` (replaces `/api/v1/auth/status`), `POST /webhooks/accounts`, the allow list, directory shares, `POST /api/v1/calls/{call}/assets/{index}/ticket` + `GET /api/v1/downloads/{ticket}` (one-time, 60 s, re-authorized at download; tickets live in memory because MCPort is single-instance). |
| S14 | Removed: `/api/v1/auth/*` and `/api/v1/iam` answer 410 `client_update_required` (pointing to `silicon-apps install mcport` and the new login commands); `/webhooks/iam` answers 410 `webhook_moved`; the Honeycomb lifecycle route is gone (404); the backend no longer serves `web/dist` (the Next.js website is a separate deployment); CORS is gone (no browser calls the API cross-origin; the BFF calls it server to server). |
| S15 | The API path stays `/api/v1`; the contract version is 0.3.0 (reported by `/health` and `/discovery`). The repo had no contract header; its own docs version the API by release, and old clients cannot sign in anyway. All crates move to 0.3.0 (`mcport-core` identity types change: breaking). |
| S16 | `mcport-core` 0.3.0: `AccountRef {uuid, id, kind, display_name, pfp_url?}`; `owner`/`caller`/`execution_account`/`account` fields replace `owner_id`/`org_id`/`environment`/`principal_id`/`actor_id`; `Connection.access`; new `Me`, `Discovery`, `AccessInput`, `ToolPolicyInput`, `DownloadTicket`, `Allowance(Input)`. `Session` stays (unchanged) only so the CLI keeps building until the CLI stage removes it. |
| S17 | The client crate, CLI and daemon received only the mechanical changes needed to build against the new wire types; their behaviour (IAM-era login and sessions, registry v1, the daemon's org check) is the next stages' work. |

### Storage, data and cutover

| # | Decision | Why |
|---|---|---|
| S18 | Storage stays the encrypted SQLite store (no Postgres). Schema steps are numbered with `PRAGMA user_version`; step 1 only adds tables (`accounts`, `identity_links`, `identity_link_runs`, `accounts_webhook_events`, `silicon_allowances`), three nullable columns on `records` (`legacy_id`, `legacy_org_id`, `legacy_owner_id`) and an index. | MCPort never used Postgres; changing engines is out of scope. Brief: new migrations only, nothing destroyed. |
| S19 | No data is rewritten at startup. Records written before 0.3.0 keep every field verbatim (storage types preserve unknown fields through typed writes) and are invisible until re-keyed. The re-key is the explicit operator command `mcport-server link-identities --file mapping.csv [--dry-run] [--offline]` (`legacy-principals` lists the ids to map). | A one-way automatic rewrite would make a rollback depend on the backup only; the explicit command can be dry-run, reviewed and re-run. |
| S20 | `link-identities` is a pure function of the original records and the mapping: it rewrites identity fields, derived record ids (grants, policies, credentials, settings), owner columns and the AES-GCM authenticated data in one IMMEDIATE transaction, records the originals (`legacy_*` columns and a `legacy` object with the exact edits), and restores a record whose principal is no longer mapped. Same mapping → no change; another mapping → recomputed from the originals. Name collisions in the new per-owner namespace are renamed `name-2` (directory: `name (2)`) and reported; two principals mapped to one uuid keep the first record and report the rest. | Brief step 7 (reversible before cutover, idempotent, report of unmatched rows). |
| S21 | Connections that were `org`-visible become `circle`; principals outside the owner's circle with evidence of use (calls, personal provider accounts, per-account tool policies) get explicit grants marked `cutover` (recomputed on every run); principals with evidence but no mapping are reported. Org-scoped rows are owned by their creator (every MCPort record has one). | Decisions file. |
| S22 | Left untouched (inert): IAM-era sessions, refresh families, login exchanges, browser attempts, IAM webhook receipts, Honeycomb environments/lifecycle receipts, account epochs, OAuth attempts, records of testing environments, and idempotency replay keys (their client keys are unknown; `recover()` makes pending calls terminal at start). A separate purge of IAM tokens is left for the Carbon to request after IAM is retired. | "Never delete existing data in a migration." Epochs only fence in-flight OAuth attempts, all of which die at cutover. |
| S23 | Host jobs keep transition fields for daemons released before 0.3.0: `principal_id` is the caller's linked pre-0.3.0 id and `org_id` the host's legacy value while the host's daemon has not reported `capabilities.registry_version = 2`; otherwise the uuid and an empty `org_id`. Per-user host account status also matches linked legacy ids for such hosts. | Survey risk: installed daemons reject jobs from "another organization" and key personal accounts by the old ids; this keeps the Carbon's running daemon working until `mcport host migrate` (CLI stage). |
| S24 | Testing environments (`--test`, `X-MCPort-Test`, `MCPORT_TEST_APP_SECRETS`) are removed; testing means a separate backend wired to a test Accounts deployment (deploy/testing.md). The `environment` column stays (`production` for every new record). | Decisions file. |

### Hardening found in review

| # | Decision | Why |
|---|---|---|
| S26 | Each account may have 30 ids resolved per minute (cached resolutions are free); more answer 429 `too_many_lookups`. | Resolving a caller-supplied id costs an Accounts lookup; without a per-caller limit one account could spend MCPort's 600/minute allowance for everyone. |
| S27 | A download ticket ends when its holder signs out everywhere after it was issued (as well as when access to the call ends). | Sign-out changes only `revoked_before`, not the account's status. |

### Local test stack

| # | Decision |
|---|---|
| S25 | mcport's webhook on the shared local Accounts stack now points at `http://127.0.0.1:4241/webhooks/accounts` (the service port of mcport's block; every update). Its `whsec_` secret was returned once and is kept, mode 0600, at `.mig/accept/webhook-secret` in this worktree; later stages can reuse it or rotate it (`POST /v1/apps/mcport/webhook/rotate-secret`). The sign-in setup was not changed. |

## Stage 2 — client crate and CLI (2026-10-10)

### Sign-in

| # | Decision | Why |
|---|---|---|
| C1 | Sign-in lives in `mcport-client` behind features: `accounts` (default; device flow, short-lived token exchange, refresh, revoke, all as MCPort's public client) and `session` (one stored sign-in, single-flight refresh). `mcport-api` stays a transport. | The contract says the CLI has no capability the package lacks; the daemon keeps depending on the small transport only. |
| C2 | The short-lived token exchange and the revoke are direct form POSTs (`grant_type=urn:silicon:params:oauth:grant-type:slt` + `client_id`; `/v1/oauth/revoke` with `token_type_hint=refresh_token` + `client_id`). Device flow and refresh use `silicon-accounts-client` 0.4.0 (`app_device_authorize`/`app_device_poll`, `refresh_app_public_client`). | 0.4.0 has no `exchange_slt_public_client` (the brief's cheat sheet names one), and its public revoke accepts only first-party client ids (brief). |
| C3 | A refused short-lived token is classified from Silicon Accounts' `error_description`: `slt_already_used`, `slt_expired`, `slt_wrong_app` (names the app), `slt_unknown`, `slt_not_a_short_lived_token`, `slt_sign_in_ended`, else `slt_refused`; each hint says how to mint a fresh one (`silicon-accounts login --app mcport -q \| mcport login --slt-stdin`). Input that does not start with `slt_` is refused before anything is sent. The token is never echoed, logged or stored. | Accounts answers every refusal with `invalid_grant`; the description is the only signal. Real refusals were checked against the local stack (progress.md). |
| C4 | One sign-in per home and backend: `<home>/.mcport/dir/accounts/<SHA-256 of ["<backend>",null]>.json` (+ `.lock`), format 1, owner-only, atomic, symbolic links refused. It records the Silicon Accounts URL and app id that issued it, so refresh and sign-out go there even if settings change. `session ls/use` is gone; separate identities use separate homes. | Decisions file ("one account per home + backend"). The key is the one mcport 0.2 used for its context directories, so host registry paths do not move. |
| C5 | Refresh when less than 60 s are left, under an exclusive lock, re-reading the file after taking it and writing the new pair before use. A 401 `token_expired`, `signed_out` or `sign_in_revoked` from the service forces one refresh and repeats the command (a 401 means nothing ran). If Silicon Accounts is briefly unreachable, a token with more than 10 s left is used anyway. When the refresh token no longer works the file is deleted and the error says why (`sign_in_ended`, with Accounts' reason). | Rotating refresh tokens: presenting a used one ends the whole sign-in. |
| C6 | `mcport login` creates the sign-in directory before it exchanges a token or waits for an approval (an unwritable home fails before a single-use token is spent), replaces this home's sign-in for the backend and revokes the replaced refresh token. `mcport logout` revokes and deletes; a failed revoke still deletes locally and says so. | The replaced sign-in can never be used again from this home. The service ignores `app_revoked` sign-outs (S5), so other machines stay signed in. |
| C7 | Before spending a code or token, `mcport login` reads the backend's public `GET /api/v1/discovery` and refuses when the backend trusts another Silicon Accounts (`accounts_mismatch`) or app id (`app_id_mismatch`), naming the fix. Best effort: an unreachable backend does not block. The CLI never adopts the backend's Accounts URL by itself. | Short-lived tokens are single use. Auto-adopting would let a backend send a Carbon to a look-alike approval page. |
| C8 | `login status`: by default refreshes if needed and confirms with `GET /api/v1/me` (`verified: true`, with the service's current id, display name and custodian), refreshing once on the same 401 codes as commands before calling a sign-in ended; `--offline` only reads the file. Signed out it prints `{"authenticated":false}`, plus `reason`/`message` only when there is something to say (a sign-in from 0.2, an unreadable file, an ended sign-in). `--json` always exits 0; without it, signed out exits 1. | Stage instructions; extra fields do not break the Silicon Apps check (`authenticated == false`). |
| C9 | `accounts --json`, `docs`, `--help` and signed-out `login status --json` run before any async runtime starts, normalize URLs without building an HTTP client, need no home and write nothing. Proven under the exact `sandbox-exec` policy of Silicon Apps' macOS runner. | The Apps sandbox allows no network, no forks and writes only to scratch; Linux workers cap processes at 32. |
| C10 | `accounts --json` = `app_id`, `client_id`, `accounts_url`, `api_url` (and `backend_url`), `website_url`, `version`, `device_flow`, `public_client`, `sign_in {carbon, silicon}`, `status`, `install`, repository/docs/package URLs. It reads saved settings when readable, so a development home shows its own URLs. | Brief shape plus what a Silicon needs to sign in without reading docs. |
| C11 | No `MCPORT_SLT` variable: mcport 0.2 had none. The positional `mcport login <token>` stays (documented, as the contracts use it); `mcport iam --json` is a hidden alias printing the `accounts --json` object, mentioned only in docs/migration. | Brief "Transition aliases". |
| C12 | No proof-authenticated calls in the client: no app calls MCPort and the service accepts no proofs yet (S7). Errors read `recovery` and, as an alias, `hint`. | Add proofs when MCPort honours its reserved scopes. |

### Commands

| # | Decision | Why |
|---|---|---|
| C13 | Removed: `session ls/use`, `--test`, `iam` (hidden alias, C11). Kept hidden with a precise error or alias: `--visibility org` → `visibility_removed` naming `--visibility circle` and `access new`; `--principal` → alias of `--account`. Added: `allow add/ls/rm [--silicon]`, `directory share/unshare/access`, `asset link`, `account show/disconnect --account <Silicon>` (custodian), `--accounts-url`/`ACCOUNTS_URL`/`config set accounts`, `login --open/--label`, `login status --offline`, `host migrate`. | D2/D5; the service stage's new routes (S13). |
| C14 | `--visibility circle` keeps the API's literal value; help and docs explain it in words ("your own people: for a Carbon, the Silicons it looks after; for a Silicon, its custodian and the custodian's other Silicons") and never use "circle" as a word in prose. `invited` stays the default. | The CLI mirrors the API values that JSON output and the website show; D2 forbids the word in copy, and a flag value is an identifier. |
| C15 | A custodian's `account disconnect --account si:…` on a local connection is refused with an explanation: local credentials live in the host's registry on that machine, so only the account itself can remove them there. Remote ones go through the service. | The service cannot reach a host's local credentials. |
| C16 | The device flow's machine label is `mcport CLI on <host name>` (from `HOSTNAME`, `COMPUTERNAME` or `/etc/hostname`, no subprocess); `--label` overrides it. `--open` is the only way the CLI opens a browser. | Stage instructions: open nothing unless asked. |

### Host registries

| # | Decision | Why |
|---|---|---|
| C17 | Registry version 2: `host.owner_uuid`; personal provider accounts keyed by uuid; no `environment`/`org_id`/`owner_id`. Version 1 still loads (old fields defaulted, each version validated on its own terms) and runs; account-changing helpers and `connection register` refuse it with `registry_not_migrated` and the two commands to run; removing a connection works on both (a deleted connection must stop running). | Survey risk: bricking existing `registry.json` files would break every local connection, including the Carbon's. |
| C18 | The daemon keys a job's personal account by `actor.principal_id` for version 1 (the service sends the old key, S23) and by `actor.uuid` for version 2, never by a public id; it reports `registry_version` and no longer rejects jobs by organization. | Matches the service's transition fields; ids can be reassigned. |
| C19 | `mcport host migrate` gets its old-key → uuid map from the service: new owner-only `GET /api/v1/hosts/{host}/legacy-accounts`, answering only for the registry's own owner key and the keys its daemon reported, through the cutover links (`link-identities`). Unlinked keys stop the migration unless `--drop-unmapped`; the original is kept as `registry.v1.json`; a running daemon is stopped and restarted; a second run reports `already_migrated`. | The operator's mapping already lives in the service; asking each host owner for a mapping file would duplicate it and leak other accounts' ids. Limiting answers to reported keys stops owners probing arbitrary ids. |
| C20 | The Carbon's own 0.2 daemon on this Mac was not touched; it keeps working through the service's transition fields until the Carbon installs 0.3.0 and runs `mcport host migrate`. | Decisions file. |

### Docs and local stack

| # | Decision |
|---|---|
| C21 | Evidence of releases before 0.3.0 (`release-readiness.md`, `MANUAL_ASSET_VERIFICATION.md`, `testing/`) moved unchanged to `docs/history/` (links fixed); current docs, help and the bundled guides name neither the old identity service nor the old distribution channel, and a unit test keeps the bundled guides that way. |
| C22 | This stage changed nothing on the shared Accounts stack: mcport's sign-in setup already had `device_flow` and `public_client`, and its webhook still points at `http://127.0.0.1:4241/webhooks/accounts` (S25). |
