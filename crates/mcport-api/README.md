# mcport-api

Stateless HTTP client and wire types for Silicon MCPort's `/api/v1`. Applications normally depend on
[`mcport-client`](../mcport-client/README.md), which re-exports this crate and adds Silicon Accounts sign-in, a stored
sign-in with single-flight refresh and explicit local host helpers.

This lower-level crate lets the host daemon use the API without depending on the public facade. `Client`,
`RequestContext` and `HostContext` hold no implicit state: each request carries the caller's Silicon Accounts access
token for the `mcport` app (`RequestContext::authenticated`) or a host's own token (`HostContext`), and no change is
retried. Responses are bounded and MCP results keep their complete JSON.

- `discovery()` (no token): the app id, the Silicon Accounts URL the service trusts, links and the contract version.
- `me()`: the signed-in Carbon or Silicon, with a Silicon's custodian.
- Connections, sharing by `c:`/`si:` id (`grant_access`, `revoke_access`), the allow list that lets an account share
  with a Silicon (`allow`, `allowances`, `disallow`), tool policies, provider accounts (`account_for` and
  `disconnect_account_for` for a custodian), MCP calls, hosts (`legacy_host_accounts` for migrating old registries),
  activity, assets and one-time download links (`asset_ticket`), directory entries and their shares, settings,
  reports and telemetry.
- `canonical_backend_url` normalizes and checks a backend URL without building an HTTP client.

Errors keep MCPort's `{"error":{"code","message","recovery","outcome_unknown"}}` (`Error::code`, `message`,
`recovery`, `status`, `is_code`, `needs_sign_in`); a `hint` field is read as the recovery. Directory updates use
`DirectoryUpdate { input, version }` with the version read from the current entry; a `DirectoryTemplate` is setup
metadata to review, never a connection or a credential.

Run `cargo test -p mcport-api` for the transport contract tests and the example.
