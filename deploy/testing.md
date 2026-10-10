# Acceptance against a test Silicon Accounts deployment

MCPort has no testing environments inside one backend. To test without touching
production, run a separate MCPort backend (its own data directory and port) wired
to a test Silicon Accounts deployment, with test Carbons and Silicons created
there. Production credentials, accounts and data are never used.

## Configure the backend

The test Accounts deployment needs an `mcport` app with `device_flow` and
`public_client` on and a registered website callback. Start the backend with:

```sh
ACCOUNTS_URL=http://localhost:9590 \      # the test deployment's public URL (token issuer)
ACCOUNTS_API_URL=http://127.0.0.1:9589 \  # only if MCPort reaches it at another address
MCPORT_APP_SECRET=<the test app's secret> \
MCPORT_ACCOUNTS_WEBHOOK_SECRET=<whsec_ of the test app's webhook> \
MCPORT_BIND=127.0.0.1:4241 MCPORT_PUBLIC_URL=http://127.0.0.1:4241 \
MCPORT_DATA_DIR=/path/to/empty/test-data \
mcport-server serve
```

`http://` is accepted only for this machine (localhost and loopback addresses).
Point the test app's webhook at `http://127.0.0.1:4241/webhooks/accounts`
(`PUT /v1/apps/mcport/webhook` with the app's credentials; the first save returns
the `whsec_` secret once) and check `POST /v1/apps/mcport/webhook/test` arrives.

## Identities and tokens

Create one Carbon, a Silicon it looks after, and an unrelated Carbon in the test
deployment. Get MCPort access tokens the way real clients do:

- Carbons: sign in through the hosted pages (the website), or approve the CLI's
  device code (`mcport login`).
- Silicons: `silicon-accounts login --app mcport -q` gives a short-lived token;
  `mcport login --slt-stdin` exchanges it (the CLI is a public client).

## What to prove

1. `GET /api/v1/me` names each account (a Silicon's `custodian` is its Carbon).
2. A Silicon's connection is managed by its custodian (`access: custodian`) and
   invisible to the unrelated Carbon; `circle` connections are usable by the owner's
   circle only.
3. Sharing with a Silicon outside the sharer's circle answers
   `silicon_not_reachable` until the custodian runs `POST /api/v1/allow`.
4. Creating a host (an introspected route) succeeds with a live sign-in.
5. Webhooks from the test deployment apply at once: an id change shows in `/me`; a
   custodian transfer moves the Silicon's activity to the new custodian; rotating
   the Silicon's STK makes its older token answer `signed_out`; removing MCPort's
   access does the same for that account until it signs in again.

On a machine that runs the Silicon Accounts testkit's local stack, `scripts/dev-accounts.sh`
sets the backend and webhook up and `scripts/e2e-accounts.sh` proves all of this (and more:
the device flow, provider accounts, host daemons, logout, refresh-token reuse, account
deletion, restart safety) with real tokens; see [tests/e2e/README.md](../tests/e2e/README.md).
The migration's runs of this procedure, including one on a copy of a database written by
MCPort 0.2.0 and re-keyed with `link-identities`, are recorded in
[docs/migration/progress.md](../docs/migration/progress.md).
