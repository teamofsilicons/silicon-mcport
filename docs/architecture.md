# Architecture

MCPort has a central Rust gateway, a stateless Rust API client, a stateful CLI with an outbound host daemon, and a React configuration website.

## Identity and permissions

The gateway exchanges an app-bound IAM SLT through the official `silicon-iam-client`. Every request introspects the current IAM application authorization. Session families bind principal, identity type, organization, application and validated testing environment. IAM refresh and gateway refresh are serialized; persisted family revocation fences logout and in-flight refresh. Browser sessions use HttpOnly cookies, same-origin checks, single-use state and typed Carbon/Silicon verification.

A connection's audience is private, organization-wide or an explicit principal list. Only its owner can change configuration, grant access, delete it or change tool policies. Invited principals still need current membership in that organization. Global and per-principal denies are checked at dispatch. Every discovered tool starts enabled unless a persisted deny applies.

Provider credentials have their own lifecycle. Shared mode selects the owner's grant; personal mode selects the caller's grant. OAuth uses protected-resource discovery, issuer binding, PKCE S256 and an exact callback. Refresh has no automatic retry after an uncertain response. Disconnect/reconnect fences pending OAuth attempts with an account epoch. OAuth currently supports RFC 8414 authorization-server metadata, a supplied public client ID or dynamic client registration. OIDC discovery fallback and Client ID Metadata Documents are not yet supported; providers requiring those paths need a compatible pre-registered client.

## Execution

Cloud HTTP calls execute centrally through bounded MCP transports. Public destinations require HTTPS, DNS validation and pinned public addresses. Redirects and environment proxies are disabled. Operators may explicitly allow exact private origins for controlled environments; clients cannot override this policy.

Local HTTP and stdio execute through a registered host. The daemon connects outward, authenticating with a host-specific token. Jobs carry connection IDs, methods, arguments and verified actor context; they never carry arbitrary endpoints, commands or provider credentials. The local registry is authoritative. Per-user process/transport sessions are isolated, bounded and invalidated when credentials or configuration change.

Before dispatch the gateway revalidates the caller's session family, membership, connection revision, account and tool policy. Local leases are durable and not re-leased. The daemon journals acceptance before execution and retries result delivery, not execution. Modern and legacy MCP negotiation are supported. Schemas are discovered in the executing account/session and validate inputs and structured outputs.

Calls record actor, selected provider account, method, timestamps, progress and outcome. Results are encrypted and caller-owned. Download indices select bytes already in a result, with current access and original tool-policy checks. Local asset materialization only fetches literally returned URLs at the configured HTTP endpoint's exact origin under `/assets/`, with strict path, byte, count and time limits. It never reads arbitrary files or follows redirects.

## Persistence and lifecycle

This version runs one gateway process over SQLite WAL with full synchronous durability. AES-256-GCM encrypts record bodies using record/tenant-bound authenticated data. The data directory is mode 0700 and key/database files are mode 0600 on Unix. Back up both the encrypted database and its protected master key. Do not deploy multiple independent gateway writers: family and dispatch coordination currently use in-process locks.

Honeycomb lifecycle requests use a dedicated service credential and bind organization, application, environment, operation, revision, generation and key version. Stale/conflicting operations fail. Durable pending/applied/completed checkpoints prevent an interrupted retry from repeating cleanup after access has resumed; a separate control revision fences login/refresh across same-generation transitions. Clean/purge removes environment data and fences work before reporting completion. Disable/restore gates access while preserving configuration. Test credentials and telemetry tables never fall back to production.

IAM webhooks use the official exact-byte signature verifier, test-envelope validation, deduplication and monotonically increasing aggregate versions. Webhooks cannot grant authority; live IAM checks remain required.

## Client boundary

`mcport-core` contains wire types. `mcport-client` retains no login state, performs no implicit refresh and never retries mutations. The CLI stores protected sessions under a backend/environment/account/organization context and serializes refresh across processes. Host-internal job endpoints are not ordinary user commands.

Unsupported interactive capabilities are explicit failures, not implied success. IAM organization membership is supported through Carbon/Silicon sessions. Generic IAM identity keys are not treated as user delegation. A separate MCPort API-key contract requires an explicit authority design.
