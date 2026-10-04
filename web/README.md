# MCPort website

A React 19 + TypeScript workspace using real open source [Arc UI](https://uiarc.dev) components. See `THIRD_PARTY_NOTICES.md` for registry source and license attribution.

```sh
npm ci
npm run dev
```

Open `http://127.0.0.1:4381`. Vite proxies `/api` to the independently running MCPort backend at `127.0.0.1:4380`. Backend configuration must set `MCPORT_WEB_URL=http://127.0.0.1:4381` so browser origin checks and IAM callbacks match. The production `dist` directory is static output; serve it with SPA fallback for `/auth/callback`, `/connections/:id/:section`, `/activity/:call`, and workspace pages, plus same-origin routing to `/api`.

```sh
npm run build
npm test
```

The website never seeds connections or presents fixture results as live data. Empty, loading, denial, unavailable-provider, and unknown-outcome states come from the real backend. `src/lib/api.ts` is the contract boundary; authoritative server DTOs and endpoints are in `../docs/API.md` and `../crates/mcport-core`.

Browser authentication uses HttpOnly session/refresh cookies. Both Carbon and Silicon flows create a typed browser attempt. The callback immediately removes token-bearing query parameters from the URL before exchanging the SLT. The opener verifies the callback origin, window reference, and attempt state. Only public actor/environment/expiry metadata is kept in tab session storage. A pasted SLT uses the same browser exchange; application secrets and session tokens are never written to JavaScript storage.

The main flows are connection discovery/search; a three-step connection wizard; tool discovery, input schemas, invocation and results; global and principal-specific restrictions; invitations; separate provider OAuth/secret authentication; hosts; call history and cancellation; telemetry preference and reports; and CLI onboarding. Tool discovery traverses provider pagination. Unknown execution outcomes are never automatically retried.

Manual release checks must exercise these flows with real backend state and IAM identities. Automated adapter tests cover cookie/identity boundaries, envelope failures, unknown-outcome handling, tool pagination, and refresh serialization; they do not replace manual browser/CLI verification or real-provider testing.

Connection sections and call details have stable URLs; browser back/forward and refresh restore the selected view. Activity opens the full caller-owned invocation, not just its summary. Result files use authenticated bounded downloads, including the active testing environment, with current server permission checks. Safe images/audio can be previewed; SVG/HTML remain downloads. Local result links copied by the daemon are shown as execution-host files.

Vercel deployment uses the explicit build-time `MCPORT_BACKEND_ORIGIN` with `npm run build:vercel`. It produces static Build Output API files and a same-origin `/api` proxy. See [Vercel setup and required origins](../deploy/vercel.md); no backend or deployment domain is selected by the repository.
