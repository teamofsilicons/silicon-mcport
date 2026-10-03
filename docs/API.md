# MCPort API contract

Application endpoints use `/api/v1`. Success is `{ "data": value }`; errors use an HTTP failure status and `{ "error": {"code", "message", "recovery": string|null, "outcome_unknown": boolean} }`. Raw asset downloads are the exception. Public DTOs live in `mcport-core`; timestamps are Unix seconds. Lists return arrays in `data`. Connection and host selectors accept IDs or unambiguous names. Reads never expose provider credentials.

Use `Authorization: Bearer <mcport-session-token>` or the website's same-origin HttpOnly session cookie. `X-MCPort-Test: <environment-id>` selects a provisioned, validated testing environment; omission means production. The header alone grants no authority. `X-MCPort-ISI` supplies optional audit context. `X-MCPort-Telemetry: false` suppresses diagnostic events for that request. JSON writes require `Content-Type: application/json`; cookie-authenticated writes must pass origin checks.

## Discovery and authentication

| Method and path | Request and response |
|---|---|
| GET `/iam` | Public `{app_id,iam_url,login_url,backend_url,website_url,repository_url,docs_url,package_url}`. |
| POST `/auth/login` | `{slt,identity_kind?:"carbon"|"silicon"}` → `Session`. Uses the official IAM client; a requested identity kind is checked against the authenticated response. |
| POST `/auth/refresh` | `{refresh_token}` → rotated `Session`. Serialized per session family; transport failures do not erase local sessions. |
| GET `/auth/status` | `{authenticated:true,actor,environment,expires_at}`, or 401. |
| POST `/auth/logout` | `{logged_out:true}`; revokes the session family and clears browser cookies. |
| GET `/auth/browser/start?identity_kind=carbon|silicon` | `{url,state}` plus a short-lived HttpOnly login-state cookie. |
| POST `/auth/browser/complete` | `{slt,state}` → public `{actor,environment,expires_at}` plus HttpOnly session and refresh cookies. **No access/refresh token in the response body.** |
| POST `/auth/browser/refresh` | Rotates the HttpOnly refresh cookie and returns the same public metadata; no JSON token is supplied. |

CLI/SDK `Session` includes `{access_token,refresh_token,expires_at,actor,environment}`. `Actor` includes `{principal_id,identity_kind,org_id,display_name}`. Browser cookies are `SameSite=Lax`, and `Secure` when deployed over HTTPS. Preserve every `Set-Cookie` header through the reverse proxy.

The browser attempt binds identity kind, environment, expiry and state. State is embedded in IAM's callback `redirect_uri`; the callback receives `slt` and `state`, removes them from its URL immediately, and exchanges them with the matching cookie. Popup messages must match the expected origin, source window and state. Only public session metadata belongs in JavaScript storage. `/auth/status` and browser refresh restore login after reload.

## Connections and access

| Method and path | Behavior |
|---|---|
| GET `/connections` | Authorized `Connection[]`. |
| POST `/connections` | `ConnectionInput` → `Connection`; any authenticated organization member may create one. |
| GET `/connections/{connection}` | Current authorized connection view and account/status metadata. |
| PATCH `/connections/{connection}` | Owner-only `ConnectionUpdate` → `Connection`; include the current `version` to detect conflicting edits. |
| DELETE `/connections/{connection}` | Owner-only → `{deleted:true}`; invalidates subsequent execution and queued work. |
| GET `/connections/{connection}/access` | Owner-only `AccessGrant[]`. |
| POST `/connections/{connection}/access` | Owner-only `{principal_id}` → `AccessGrant`; invite a current member of the same organization. |
| DELETE `/connections/{connection}/access/{principal}` | Owner-only → `{deleted:true}`. |
| GET `/connections/{connection}/policies` | `ToolPolicy[]`; owners see all, other callers see applicable policies. |
| PUT `/connections/{connection}/policies` | Owner-only `{tool,principal_id:string|null,enabled}` → `ToolPolicy`. A principal-specific allow cannot override a connection-wide deny. |

`ConnectionInput`: `{name,description?,transport:"http"|"stdio",url?,host_id?,command?,args?:[],auth_mode:"none"|"per-user"|"shared",visibility:"private"|"org"|"invited"}`. HTTP requires a URL; stdio requires an owned host and command. A host identifies local HTTP execution too. Remote HTTP uses public upstream hosts unless the operator explicitly configures an exception. Names are unique per organization/environment. Provider credentials are configured separately.

`ConnectionUpdate` accepts `name`, `description`, `visibility`, and `version`. Use permission does not grant management permission. Visibility, organization membership, provider account and tool policy are checked again before execution.

Local status separates the host heartbeat from the MCP protocol check: `offline` means the host/registration is unavailable or a recent MCP check failed, `checking` means no fresh protocol result, and `authentication_required` means the caller's execution account is missing. `ready` requires a recent successful check. Protocol health is advisory; an authorized caller can retry within the ordinary execution timeout. Health checks use only registered endpoints and the selected account, never tool calls.

## Provider accounts

| Method and path | Behavior |
|---|---|
| GET `/connections/{connection}/account` | Secret-free `AccountStatus`: `{connected,owner_id,label,kind}`. |
| POST `/connections/{connection}/account` | `{kind:"bearer"|"header",secret,label?,header_name?}` → `AccountStatus`. Per-user grants belong to the caller; only the owner configures a shared grant. |
| DELETE `/connections/{connection}/account` | `{disconnected:true}`; caller's own grant, or owner-only for shared grants. |
| POST `/connections/{connection}/account/authorize` | `{client_id?:string}` → `{authorization_url,state}`. Discover OAuth metadata and prepare PKCE; providers may require a pre-registered client. |

Remote credentials require HTTPS and never travel in URLs. The returned authorization URL opens `/oauth/start` for account/sharing confirmation before provider consent; `/oauth/callback` checks the attempt-bound issuer, resource, owner and state. Local execution credentials stay in the host registry. Configure or disconnect them through the CLI on that host, not by uploading secrets to the gateway.

Public `GET /oauth/client-metadata.json` (outside `/api/v1`) serves Client ID Metadata when the gateway has a configured public HTTPS URL. Its client ID and exact callback come from configuration; request Host headers cannot change them. Registration supports an explicit public client ID, advertised metadata registration, or dynamic client registration. Confidential clients and automatic runtime scope upgrades are unsupported.

## MCP execution and history

POST `/connections/{connection}/mcp` accepts `RpcInput`:

```json
{"method":"tools/call","params":{"name":"example","arguments":{}},"timeout_ms":60000,"idempotency_key":"one-logical-operation"}
```

It returns `RpcOutput`: `{call_id,result}`. Supported methods are `tools/list`, `tools/call`, `resources/list`, `resources/templates/list`, `resources/read`, `prompts/list`, `prompts/get`, and `completion/complete`. Completion passes MCP `ref`, `argument`, and optional `context` in `params`; provider capability checks still apply.

Tool discovery retains schema/annotation metadata and adds effective `enabled` flags. Tool input and declared structured output schemas are validated. Pagination cursors remain in the result. Complete provider content is preserved, including text, structured content, images, audio and resource references. Provider tool errors retain `isError`; the CLI exits nonzero. `timeout_ms` is bounded to 100–600000 ms (default 120000).

An idempotency key is scoped to the caller, organization, environment and connection. Reusing it with an identical method/params returns the stored outcome; a different request conflicts. Already-running work returns `operation_in_progress`. Unknown outcomes are never automatically replayed: inspect history/provider state before starting another action.

| Method and path | Behavior |
|---|---|
| GET `/calls?connection_id=` | Caller-owned `Invocation[]` summaries; result bodies are omitted. |
| GET `/calls/{id}` | Full caller-owned invocation plus latest `progress`; current connection/tool access and environment validity are rechecked. |
| POST `/calls/{id}/cancel` | Caller-owned invocation; cancellation is best effort and cannot undo completed provider effects. No result disclosure after access revocation. |
| GET `/calls/{id}/assets` | `ResultAsset[]` from already-returned content, with current caller/access/policy checks. |
| GET `/calls/{id}/assets/{index}` | Raw bytes, attachment headers, `nosniff`, restrictive CSP and `no-store`; SVG/HTML use `application/octet-stream`. |

`Invocation` records caller, execution account, connection, method/tool, status, timestamps, result and error. Progress is the latest provider notification, not a guarantee of total completion. A pending or running invocation may end completed, failed, cancelled or unknown.

`ResultAsset` contains `{index,name,mime_type,size,source_uri,download_url}`. Download URLs contain no credentials; use the same authenticated actor and environment. Downloads never accept arbitrary URLs or filesystem paths. The daemon may copy exact same-origin `/assets/` links returned by local HTTP MCP calls, within strict path, timeout and size bounds; see [ASSETS.md](ASSETS.md).

## Hosts and daemon transport

| Method and path | Behavior |
|---|---|
| GET `/hosts` | Caller-owned `Host[]`. |
| POST `/hosts` | `{name}` → `HostRegistration`; `host_token` is returned only once. |
| GET `/hosts/{id}` | Owner's host metadata/status. |
| DELETE `/hosts/{id}` | `{deleted:true}`; revokes host authority and cancels dispatch. |
| POST `/hosts/{id}/poll` | `HostPoll` → `HostPollResult`; bounded long poll, up to 20 seconds. |
| POST `/hosts/{id}/jobs/{job}/result` | `HostJobResult` → `{accepted:true}`; bound host/job and idempotent completion. |
| POST `/hosts/{id}/jobs/{job}/progress` | `{progress:Value}` → `{accepted:true}`. |

Connector methods use a host bearer token, never a user session. Its host ID and environment must match; optional `X-MCPort-Test` must agree. The Rust SDK exposes `Client::host_poll`, `host_result`, and `host_progress` with explicit `HostContext {host_id,host_token,environment,isi}`; it does not persist this authority.

`HostPoll` contains `registered_connections` and `capabilities`; the daemon reports capacity, active count/IDs and available local account capabilities. Only jobs matching that host's approved local registry are leased. Jobs contain identity, connection ID, MCP method/params and deadline; they never install endpoints, commands or credentials remotely. The gateway rechecks caller authorization before dispatch, and the daemon checks local registration/deadlines. `cancelled` identifies active jobs to interrupt. Started side-effect jobs are not leased again after reconnect.

## Settings, telemetry and reports

- GET/PATCH `/settings` → `{telemetry:boolean}`; default true, scoped by actor/organization/environment. PATCH accepts that same shape.
- POST `/telemetry` `{source,operation,step,outcome,progress?,correlation_id?,duration_ms?}` → `{recorded:boolean}`. Sources are `web`, `cli`, `daemon`, `backend`, or `rust-client`. Operations and steps use fixed allowlists, progress is 0–1, correlation IDs are UUIDs, and duration is bounded. No arbitrary tool input, result, credential or free-text event payload is accepted. Opt-out and absent environment-specific delivery configuration return `recorded:false`.
- POST `/reports` `{message,pr?:string}` → `{id,status,delivery_detail,repository_url}`. Explicit user text only; never automatic credentials/logs. A production durable outbox reports `delivery_pending` or `delivery_failed`, not a false sent acknowledgement. Testing reports return `test_recorded` and never enter production mail delivery.

## Honeycomb control plane

These service endpoints are outside `/api/v1` and use their own receipt format:

- PUT `/internal/honeycomb/organizations/{org}/testing-environments/{environment}/operations/{operation}` applies an authenticated lifecycle operation; GET reads its receipt. A dedicated service bearer token is required. Path/body app, org, environment and operation identifiers must agree. Repeating the identical operation returns its receipt; stale/conflicting revisions are rejected.
- Actions cover prepare/import, disable/restore, clean, purge, key rotation and application retirement. Generation/revision fences invalidate stale sessions, browser/OAuth attempts and work. Clean/purge remove the appropriate testing state. A request header never provisions an environment.
- POST `/webhooks/iam` verifies exact signed bytes through the official IAM verifier, including timestamp, version and testing context. Events are deduplicated and versioned. The payload cannot overwrite authority; user requests and dispatch use current IAM introspection.

Development uses backend `127.0.0.1:4380` and website `127.0.0.1:4381` with `/api` proxying. [Manual evidence](testing/manual.md) distinguishes public-provider results, official local MCP behavior and controlled fixtures; these are not production deployment claims.
