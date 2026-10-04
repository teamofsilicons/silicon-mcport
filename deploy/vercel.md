# Website on Vercel

The website can run as a static Vercel deployment with `/api` proxied to the separately hosted Rust backend. This preparation chooses no backend, Vercel project or domain and deploys nothing.

Use project **Root Directory `web`**, Node.js **24**, framework **Other**, install command `npm ci`, and build command `npm run build:vercel`. Keep Output Directory unset: the build writes Vercel's `.vercel/output` format rather than deploying `dist` directly. This follows the existing Honeycomb/Hook workspace convention and Vercel's [Build Output API](https://vercel.com/docs/build-output-api/configuration).

Supply these values after the backend server and frontend origin are chosen:

| Setting | Where | Value |
|---|---|---|
| `MCPORT_BACKEND_ORIGIN` | Vercel build environment | Reachable backend HTTPS origin, without credentials, path, query or fragment. No default is selected. |
| `MCPORT_PUBLIC_URL` | Rust backend environment | Backend's public HTTPS origin, used by CLI discovery, provider OAuth and client metadata. |
| `MCPORT_WEB_URL` | Rust backend environment | Exact stable Vercel/custom frontend HTTPS origin, with no trailing slash or path. |
| IAM browser callback | MCPort application registration | The frontend origin followed by `/auth/callback`, for the supported Carbon/Silicon login flows. |

The backend still needs its IAM application configuration, protected persistent storage and service secrets from `environment.example`. None of those secrets belong in the frontend or a `VITE_*` variable. Vercel receives only the chosen backend origin for these routing rules. Configure it separately for each deployment environment and rebuild after changing it.

To verify the output locally after choosing that origin:

```sh
cd web
npm ci
# Set MCPORT_BACKEND_ORIGIN in this build environment first.
npm run build:vercel
npm test
```

The build rejects missing or malformed backend configuration. It prepares files only. A successful local build does not prove backend reachability or Vercel cookie forwarding.

## Auth and routing

The browser keeps calling same-origin `/api/v1/...` with HttpOnly cookies. [External rewrites](https://vercel.com/docs/routing/rewrites) send `/api` to the configured backend before any static-file or SPA fallback. API responses are `private, no-store` and CDN caching is disabled. Static assets use immutable caching; callbacks and page responses use `no-store`. Missing assets stay 404 instead of returning HTML.

The proxy must preserve the browser's `Origin`, cookies, `X-MCPort-Test`, and every separate `Set-Cookie` header. Login completion, refresh and logout each return multiple cookies. Host-only cookies then belong to the frontend origin; no cross-site cookie or broad CORS exception is needed. Provider consent and callback remain on the backend's `/oauth/start` and `/oauth/callback`; do not rewrite them to the SPA. Keep the backend's attachment response headers intact.

The existing website requests a 60-second tool deadline. Vercel's external proxy [waits up to 120 seconds for the initial response](https://vercel.com/changelog/cdn-origin-timeout-increased-to-two-minutes). A proxy interruption can still leave a tool outcome unknown: inspect Activity and the provider before retrying. An unstructured proxy 502/504 now preserves that warning without replaying the tool call. Longer CLI requests should use the backend directly.

## Preview and verification

A fully working preview cannot be made independently of a backend. Local `npm run build` can prepare the static UI, but login and MCP actions require a reachable backend configured for the exact frontend origin. Use a stable preview alias with a dedicated backend configuration. An arbitrary per-commit `*.vercel.app` URL will not match the fixed callback/origin checks; do not add wildcard trust to bypass this.

After an authorized deployment, verify the actual URL: reload a connection deep link and `/auth/callback`; check `/api/v1/iam` returns backend JSON; complete Carbon/Silicon login, refresh, logout and re-login; confirm all session cookies stay HttpOnly/Secure; run a real tool and download a result; and inspect API cache headers. Include a controlled proxy failure when checking unknown-outcome recovery. Current checks cover generated configuration and adapter behavior, not a live Vercel deployment.
