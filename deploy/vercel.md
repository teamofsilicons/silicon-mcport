# Website on Vercel

The website at `https://mcport.teamofsilicons.com` is a Next.js app in `web/`, deployed
to the existing Vercel project. It is a backend for the browser: its server signs Carbons
in through Silicon Accounts' hosted pages, keeps the sign-in in a sealed httpOnly cookie,
refreshes it, and calls the MCPort service with `Authorization: Bearer`. The browser
only ever talks to the website's own origin and never sees a token or the app secret.
Silicons can also sign in there with a short-lived token from
`silicon-accounts login --app mcport -q`; the website's server exchanges it.

The service at `https://api.mcport.teamofsilicons.com` deploys separately
([README.md](README.md)); it no longer serves the website.

## Project settings

| Setting | Value |
|---|---|
| Root Directory | `web` |
| Framework Preset | Next.js |
| Node.js | 24 |
| Install / build | the preset's defaults (`pnpm install --frozen-lockfile`, `pnpm build`) |
| Output | the preset's default; no rewrites, no Build Output API |

Every value below is read per request, so one build serves any environment.

## Environment variables

Set them for Production. Set them for Preview only with a fixed preview domain whose
callback is registered (an arbitrary per-commit `*.vercel.app` URL is not, and no
wildcard is ever registered). Mark `APP_SECRET` and `SESSION_SECRET` sensitive.

| Variable | Production value |
|---|---|
| `APP_ID` | `mcport` |
| `APP_SECRET` | the `mcport` app secret from Silicon Accounts: the same secret the service holds as `MCPORT_APP_SECRET`, copied from Secrets Manager `silicon-mcport/production-runtime`; never in the repository |
| `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com` |
| `APP_API_URL` | `https://api.mcport.teamofsilicons.com` |
| `PUBLIC_URL` | `https://mcport.teamofsilicons.com` |
| `SESSION_SECRET` | a new `openssl rand -base64 48` for this environment only |
| `ACCOUNTS_API_URL`, `EXTRA_IMG_ORIGINS` | leave unset |

The variables of the previous static website (`MCPORT_BACKEND_ORIGIN` and any `VITE_*`)
are no longer read; delete them from the project.

## Silicon Accounts

The `mcport` app's sign-in setup must list `https://mcport.teamofsilicons.com/auth/callback`
in `redirect_uris` and `https://mcport.teamofsilicons.com` in `allowed_origins`. Arrays
replace, so read the current setup and send it back whole with its version; the exact
commands are in the [cutover runbook](../docs/migration/cutover.md).

## Headers

The site sets its own Content Security Policy on every page: scripts by nonce,
`connect-src 'self'` (the browser calls only this site), images from this site, Silicon
Accounts (`ACCOUNTS_URL`, for profile photos) and Iris, `frame-ancestors 'none'`. Add no
Vercel header rules that weaken it. API answers pass through the site's `/api` routes
with `Cache-Control: private, no-store`.

## Large results

Tool results and files stay on the service. For a file, the website asks the service for
a one-time, 60-second ticket (`POST /api/v1/calls/{call}/assets/{index}/ticket`) and the
browser downloads it straight from the service's absolute ticket URL
(`https://api.mcport.teamofsilicons.com/api/v1/downloads/{ticket}`), so no body
larger than Vercel's function limit passes through the website. Small JSON goes through
the site's `/api` proxy. Tool calls can take up to the service's deadline; a call interrupted by the network
has an unknown outcome: check Activity before repeating it.

## Verify a deployment

After an authorised deployment, on the real URL: sign in as a Carbon through Silicon
Accounts, reload a connection deep link, sign out and back in; sign in as a Silicon with a
short-lived token; run a tool and download a result; check that `/api/v1/me` (through the
site) names the account and that no response sets a token in a cookie readable by scripts.
`grep -r sa_app_ .next/static` on the build output must find nothing. Local builds prove
the configuration, not a live deployment.
