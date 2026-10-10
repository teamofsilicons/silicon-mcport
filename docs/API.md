# MCPort API contract

API contract **0.3.0** (`GET /health` and `GET /api/v1/discovery` report it as `version`). Application endpoints use
`/api/v1`. Success is `{ "data": value }`; errors use an HTTP failure status and
`{ "error": {"code", "message", "recovery": string|null, "outcome_unknown": boolean} }`. Raw asset downloads are the
exception. Public DTOs live in `mcport-core`; timestamps are Unix seconds. Lists return arrays in `data`. Connection and
host selectors accept IDs or unambiguous names. Reads never expose provider credentials. A machine-readable copy of
this contract is [openapi.yaml](openapi.yaml).

New connection, host, invocation/job, report and directory IDs use case-sensitive base62 (`a-z`, `0-9`, `A-Z`),
starting at three characters. These resource types share one persistent namespace per database. After all 238,328
three-character values have been used or reserved, IDs grow to four characters, then continue growing only after each
complete width is exhausted. Deleted IDs are never reused. Treat IDs as opaque locators, not secrets: authorization is
still required. Existing UUID and legacy invocation IDs remain valid. Account uuids, tokens, OAuth state and telemetry
UUIDs keep their own formats.

## Accounts and authentication

Every Carbon and Silicon is a [Silicon Accounts](https://accounts.teamofsilicons.com) account. MCPort stores the
account's permanent **uuid** (short and case-sensitive, e.g. `zQo`) and shows its current **id** (`c:ada`, `si:scout`),
which can change. Wherever a request names an account, it accepts a `c:`/`si:` id (resolved through Silicon Accounts;
current ids only) or a uuid. Responses describe accounts as `AccountRef`:
`{uuid, id, kind: "carbon"|"silicon", display_name, pfp_url?}`.

Send `Authorization: Bearer <access token>` with a Silicon Accounts access token issued to the `mcport` app (EdDSA JWT,
`aud` = `mcport`, `iss` = the Accounts public URL, 30 minutes). MCPort verifies it locally against the Accounts JWKS
(cached; refetched for an unknown key id at most every 30 seconds) and refuses tokens issued before the account signed
out everywhere, rotated its key or removed MCPort's access. Routes marked **live** also confirm the sign-in with
Silicon Accounts (introspection, reused for 30 seconds), so revocation applies to them at once. MCPort issues no
sessions or cookies; the website's server holds the sign-in and sends the bearer token.

How to get a token: Carbons run `mcport login` (device flow) or sign in on the website; Silicons run
`silicon-accounts login --app mcport -q | mcport login --slt-stdin` (the CLI exchanges the short-lived token as a public
client). `Authorization: Proof sap_…` (proofs issued by other apps) is refused with `proof_not_accepted`: MCPort honours
no proof scopes yet and reserves `mcport.connections.read` and `mcport.tools.call` for a later release.

Authentication errors (401): `authentication_required` (no bearer token), `token_expired`, `wrong_audience`,
`wrong_issuer`, `unknown_signing_key`, `token_not_yet_valid`, `invalid_token`, `signed_out` (issued before a
revocation), `account_deleted`, `sign_in_revoked` (live check failed). 503 `accounts_unavailable` means MCPort could
not reach Silicon Accounts. Naming accounts costs lookups: each account may have 30 ids resolved per minute (429
`too_many_lookups` beyond). `X-MCPort-ISI` supplies optional audit context. `X-MCPort-Telemetry: false` suppresses
diagnostic events for that request. JSON writes require `Content-Type: application/json`.

| Method and path | Request and response |
|---|---|
| GET `/discovery` | Public `{app_id, accounts_url, client_id, backend_url, website_url, repository_url, docs_url, package_url, install_url, version}`. `client_id` is the app id public clients use. |
| GET `/me` | `{uuid, id, kind, display_name, pfp_url?, custodian: AccountRef|null, expires_at}`; `custodian` is set for Silicons. |

Routes of releases before 0.3.0 (`GET /iam`, everything under `/auth/`) answer **410** `client_update_required`.

## Ownership, sharing and the circle

Each connection, host and directory entry belongs to the account that created it.

- **Custodian.** The Carbon that looks after a Silicon manages that Silicon's connections and directory entries, may
  list, show and delete its hosts, sees its activity and results, and may inspect and disconnect its personal provider
  accounts. It never acts as the Silicon: its own calls are recorded as its own.
- **Circle.** A Carbon's circle is the Carbon and the Silicons it looks after; a Silicon's circle is the Silicon, its
  custodian and the custodian's other Silicons. A connection with visibility `circle` is usable by the owner's circle.
  Circles are derived from Silicon Accounts custodian data on every request (cached; updated by webhook and lookups).
- **Sharing.** Owners (or custodians of Silicon owners) give use access to specific accounts by id. Use never grants
  management.
- **Silicons are not open to the world.** Sharing with a Silicon outside the sharer's circle fails with 403
  `silicon_not_reachable` until the Silicon, or its custodian, allows the sharer (`/allow`). Carbons can be reached by
  anyone signed in.

| Method and path | Behavior |
|---|---|
| GET `/allow?silicon=` | The allow list of the caller (a Silicon) or of a Silicon the caller looks after: `Allowance[]` (`{silicon, account, created_at, created_by}`). |
| POST `/allow` **live** | `{account, silicon?}` → `Allowance`. The Silicon itself, or its custodian. |
| DELETE `/allow/{account}?silicon=` **live** | `{deleted:boolean}`. |

## Directory

Directory entries describe reusable setup. They never include provider credentials, host registrations, or access
grants and never execute a server. The bundled community snapshot is read-only and visible to everyone signed in.
Personal entries are visible to their creator, the creator's custodian, and accounts they are shared with; the creator
and its custodian manage them. Each account can keep 1000 entries. Selecting one copies settings into an ordinary
connection; later directory edits do not change it.

| Method and path | Behavior |
|---|---|
| GET `/directory?q=` | Search visible community and personal entries; returns `DirectoryEntry[]`. |
| POST `/directory` | `DirectoryInput` → personal `DirectoryEntry`. |
| GET `/directory/{id}` | Visible entry by ID. |
| PUT `/directory/{id}` | Manager-only `{input:DirectoryInput,version:i64}` → updated entry; a stale version returns 409. |
| DELETE `/directory/{id}` | Manager-only → `{deleted:true}`; configured connections remain. |
| GET `/directory/{id}/access` | Manager-only `AccessGrant[]`. |
| POST `/directory/{id}/access` **live** | Manager-only `{account}` → `AccessGrant`. |
| DELETE `/directory/{id}/access/{account}` **live** | Manager-only → `{deleted:true}`. |

`DirectoryInput`: `{name,description?,category?,source_url?:string|null,template?:DirectoryTemplate|null}`.
`DirectoryTemplate`: `{transport:"http"|"stdio",url?:string|null,command?:string|null,args?:[],auth_mode:"none"|"per-user"|"shared"}`.
Templates may omit machine-specific values; normal connection validation applies at creation. Source and endpoint URLs
cannot embed credentials. `DirectoryEntry` includes the input plus
`{id,source:"community"|"personal",source_revision:string|null,owner:AccountRef|null,can_manage,version,created_at,updated_at}`.

## Connections and access

| Method and path | Behavior |
|---|---|
| GET `/connections` | `Connection[]` the caller can use. |
| POST `/connections` | `ConnectionInput` → `Connection`; the caller becomes the owner. |
| GET `/connections/{connection}` | Current connection view with account/status metadata. |
| PATCH `/connections/{connection}` **live** | Manager-only `ConnectionUpdate` → `Connection`; include the current `version` to detect conflicting edits. |
| DELETE `/connections/{connection}` **live** | Manager-only → `{deleted:true}`; invalidates subsequent execution and queued work. |
| GET `/connections/{connection}/access` | Manager-only `AccessGrant[]` (`{account, created_at, created_by: AccountRef|null}`). |
| POST `/connections/{connection}/access` **live** | Manager-only `{account}` (`principal_id` is accepted as the same field) → `AccessGrant`. |
| DELETE `/connections/{connection}/access/{account}` **live** | Manager-only → `{deleted:true}`. |
| GET `/connections/{connection}/policies` | `ToolPolicy[]` (`{tool, account: AccountRef|null, enabled}`); managers see all, others the ones that apply to them. |
| PUT `/connections/{connection}/policies` **live** | Manager-only `{tool, account: string|null, enabled}` → `ToolPolicy`. An account-specific allow cannot override a connection-wide deny. |

Managers are the owner and, for a Silicon owner, its custodian. `Connection` is `{id, name, description, owner:
AccountRef, transport, url?, host_id?, command?, args, auth_mode, visibility, status, can_manage, access, account:
AccountStatus, created_at, updated_at, version}`; `access` says why the caller sees it: `owner`, `custodian`, `circle`
or `invited`. URL, command and arguments are shown to managers only.

`ConnectionInput`: `{name,description?,transport:"http"|"stdio",url?,host_id?,command?,args?:[],auth_mode:"none"|"per-user"|"shared",visibility?:"invited"|"circle"}`.
HTTP requires a URL; stdio requires a host the caller registered and an absolute command. Remote HTTP uses public
upstream hosts unless the operator configures an exception. Names are unique per owner. Visibility defaults to
`invited` (the owner and the accounts it adds; with no grants it is owner-only). `private` is accepted as `invited`; an
update to `private` atomically removes existing grants. `org` is refused with 400 `visibility_removed`.

Name resolution: an exact ID the caller can use wins; otherwise the caller's own connection with that name; otherwise
one connection it can use. Several candidates answer 409 `ambiguous_name` listing IDs and owners.

Local status separates the host heartbeat from the MCP protocol check: `offline` means the host/registration is
unavailable or a recent MCP check failed, `checking` means no fresh protocol result, and `authentication_required` means
the caller's execution account is missing. `ready` requires a recent successful check. Health checks use only
registered endpoints and the selected account, never tool calls.

## Provider accounts

| Method and path | Behavior |
|---|---|
| GET `/connections/{connection}/account?account=` **live** | Secret-free `AccountStatus`: `{connected, account: AccountRef|null, label, kind}`. `account=` names a Silicon the caller looks after. |
| POST `/connections/{connection}/account` **live** | `{kind:"bearer"|"header",secret,label?,header_name?}` → `AccountStatus`. Per-user accounts belong to the caller; only managers configure a shared account. Custodians never connect one for a Silicon. |
| DELETE `/connections/{connection}/account?account=` **live** | `{disconnected:true}`; the caller's own per-user account, a Silicon's one by its custodian, or (managers) the shared account. |
| POST `/connections/{connection}/account/authorize` **live** | `{client_id?:string}` → `{authorization_url,state}`. Discovers OAuth metadata and prepares PKCE. |

Remote credentials require HTTPS and never travel in URLs. The returned authorization URL opens `/oauth/start` (a
confirmation page naming the account by its current id) before provider consent; `/oauth/callback` checks the
attempt-bound issuer, resource, account and state, and re-checks that the account has not signed out since. Local
execution credentials stay in the host registry. Public `GET /oauth/client-metadata.json` (outside `/api/v1`) serves
Client ID Metadata when the service has a public HTTPS URL. Provider OAuth is unrelated to Silicon Accounts sign-in.

## MCP execution and history

POST `/connections/{connection}/mcp` **live** accepts `RpcInput`:

```json
{"method":"tools/call","params":{"name":"example","arguments":{}},"timeout_ms":60000,"idempotency_key":"one-logical-operation"}
```

It returns `RpcOutput`: `{call_id,result}`. Supported methods are `tools/list`, `tools/call`, `resources/list`,
`resources/templates/list`, `resources/read`, `prompts/list`, `prompts/get`, and `completion/complete`. Tool discovery
adds effective `enabled` flags. Tool input and declared output schemas are validated. Provider tool errors keep
`isError`. `timeout_ms` is bounded to 100–600000 ms (default 120000). An idempotency key is scoped to the caller and
connection; reusing it with an identical request returns the stored outcome and a different request conflicts. Unknown
outcomes are never replayed automatically.

| Method and path | Behavior |
|---|---|
| GET `/calls?connection_id=` | `Invocation[]` summaries (no result bodies) made by the caller or by Silicons it looks after. |
| GET `/calls/{id}` | Full invocation plus latest `progress`; the caller's current access to the connection and tool is re-checked. |
| POST `/calls/{id}/cancel` | Best-effort cancellation by the caller or its custodian. |
| GET `/calls/{id}/assets` | `ResultAsset[]` from already-returned content, with current access checks. |
| GET `/calls/{id}/assets/{index}` | Raw bytes, attachment headers, `nosniff`, restrictive CSP and `no-store`; SVG/HTML use `application/octet-stream`. |
| POST `/calls/{id}/assets/{index}/ticket` | `{url, expires_at}`: a one-time link valid for 60 seconds that downloads the asset without a token (for browsers). |
| GET `/downloads/{ticket}` | Redeems a ticket once; authorization is checked again at download time. 404 `download_expired` when used or expired. |

`Invocation` is `{id, connection_id, connection_name, caller: AccountRef, execution_account: AccountRef|null, method,
tool_name, status, created_at, completed_at, result, error}`. Results belong to the caller: connection owners do not
see other accounts' calls. Revoking the caller's access also hides its old results.

## Hosts and daemon transport

| Method and path | Behavior |
|---|---|
| GET `/hosts` | `Host[]` the caller owns or looks after (`{id, name, owner, online, last_seen, created_at, can_manage}`). |
| POST `/hosts` **live** | `{name}` → `HostRegistration`; `host_token` is returned only once. |
| GET `/hosts/{id}` | Host metadata/status (owner or custodian). |
| DELETE `/hosts/{id}` **live** | `{deleted:true}`; revokes host authority and cancels dispatch (owner or custodian). |
| POST `/hosts/{id}/poll` | `HostPoll` → `HostPollResult`; bounded long poll, up to 20 seconds. |
| POST `/hosts/{id}/jobs/{job}/result` | `HostJobResult` → `{accepted:true}`. |
| POST `/hosts/{id}/jobs/{job}/progress` | `{progress:Value}` → `{accepted:true}`. |

Daemon routes authenticate with the host token (`Authorization: Bearer mph_…`), never an account token. Only the host's
owner adds connections to it. A job's `actor` names the caller (`uuid`, `id`, `kind`, `display_name`) and repeats
`principal_id`, `identity_kind` and `org_id` for daemons released before 0.3.0: for a host registered before 0.3.0 whose
daemon still uses its old registry, `principal_id` is the caller's pre-0.3.0 id and `org_id` the registry's value;
otherwise `principal_id` is the uuid and `org_id` is empty. A daemon that keys personal accounts by uuid reports
`capabilities.registry_version = 2`. The `X-MCPort-Test` header of older daemons is ignored.

## Settings, telemetry and reports

- GET/PATCH `/settings` → `{telemetry:boolean}`; default true, per account.
- POST `/telemetry` `{source,operation,step,outcome,progress?,correlation_id?,duration_ms?}` → `{recorded:boolean}`.
  Fixed allowlists; no tool input, result, credential or free text. Events carry the account uuid and kind.
- POST `/reports` `{message,pr?:string}` → `{id,status,delivery_detail,repository_url}`. The maintainers' email names the
  reporter by current id and uuid. Text that looks like a credential (bearer tokens, `sar_`, `sap_`, `whsec_`, STKs,
  JWTs, older MCPort tokens) is refused.

## Silicon Accounts webhook

POST `/webhooks/accounts` (outside `/api/v1`) receives Silicon Accounts events for the `mcport` app. The signature
(`X-Accounts-Timestamp`, `X-Accounts-Signature: v1=…`, HMAC-SHA256 of `"{timestamp}.{raw body}"` with
`MCPORT_ACCOUNTS_WEBHOOK_SECRET`) is checked over the exact body bytes with a 5-minute tolerance: 401
`invalid_webhook_signature` otherwise, 400 `invalid_webhook_body` for a body that is not an event, 503
`webhook_not_configured` without a secret. Each `event_id` is handled once (a duplicate answers
`{received:true,duplicate:true}`); changes older than what MCPort already applied are ignored.

| Event | Effect |
|---|---|
| `account.id_changed`, `account.updated` | Update the displayed id and profile (`account.version` orders updates). |
| `silicon.custodian_changed` | The new custodian (and its circle) gets access at once; the previous one loses it. |
| `membership.signed_out` | Reason `app_revoked` (one MCPort sign-out) is ignored. Any other reason refuses tokens issued before the event and cancels the account's pending calls and provider authorizations. |
| `membership.access_removed` | As above, and what the account owns or shares is hidden from others until it signs in again. |
| `account.deleted` | As above, then the account's connections (with their grants, policies and provider accounts), hosts, directory entries, calls, settings, reports and personal provider accounts are deleted; its access to others' connections is removed; others' data stays. |
| `ping`, unknown types | Acknowledged. |

`POST /webhooks/iam` answers 410 `webhook_moved`.
