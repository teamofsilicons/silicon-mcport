# Silicon MCPort website

Next.js 16, React 19 and Arc UI, using the Accounts/Apps theme and workspace shell. The website calls the actual MCPort API through a same-origin server proxy. Tokens stay in sealed HTTP-only cookies; the browser uses hosted Silicon Accounts sign-in with PKCE.

## Product pages

- Connections: cloud/local HTTP or stdio setup, discovery and paginated tools, global/per-account policy, JSON input/schema, execution and structured text/media/resource/file results.
- Provider accounts: no-auth, personal and shared credentials; OAuth authorization, bearer/header credentials and disconnect. Custodians can inspect and disconnect a Silicon's personal provider account.
- Sharing: exact Carbon/Silicon grants, invited or custodial visibility, personal directory entry sharing and inbound Silicon allowance management.
- Directory: searchable community/personal templates, create/edit/delete and connection setup from a template. Hosts: registration instructions, live availability and removal.
- Resources/templates and prompts, activity inspection/cancellation/download, configuration with optimistic version checks, telemetry and bug reports.

## Local setup

Use Node 24+ and the pinned pnpm version in package.json. From the repository root, configure and start `scripts/dev-accounts.sh` using `docs/migration/progress.md`. The backend listens on `4241`. Then:

```sh
cd web
corepack enable
pnpm install --frozen-lockfile
cp .env.example .env.local
# Fill APP_SECRET and SESSION_SECRET for the registered mcport app.
pnpm dev
```

Open `http://127.0.0.1:4240`. Register this exact origin plus `/auth/callback` in Silicon Accounts. Local email codes are delivered to the shared test messaging service. The API must already run; the website does not start a product stub.

| Variable | Purpose |
| --- | --- |
| `APP_ID` | Defaults to `mcport`. |
| `APP_SECRET` | Silicon Accounts app credential, server only. |
| `ACCOUNTS_URL` | Hosted sign-in origin and token issuer. |
| `ACCOUNTS_API_URL` | Optional private Accounts API origin. |
| `APP_API_URL` | Product API origin (`http://127.0.0.1:4241` locally). |
| `SESSION_SECRET` | At least 32 random bytes for sealed cookies; rotation signs browsers out. |
| `PUBLIC_URL` | Website origin; its `/auth/callback` must be registered. |
| `EXTRA_IMG_ORIGINS` | Optional image/media origins for local fixtures or account photos. |
| `EXTRA_ORIGINS` | Optional additional origins of this website for write requests. |

Values are validated at server startup and read at request time. External production origins require HTTPS. Cookies use Secure and the __Host prefix on HTTPS. The proxy forwards only allowlisted headers and refreshes access tokens on the server; state-changing requests require the site's Origin. Browser paths `/api/api/v1/...` forward to the backend's `/api/v1/...`.

## Verification

```sh
pnpm typecheck
pnpm lint
pnpm test
# Real backend plus shared Accounts stack must be running:
TEST_STACK_JSON=/path/to/test-stack.json pnpm test:e2e
NEXT_OUTPUT=standalone pnpm build
```

Playwright starts Next on 4240, signs two fresh Carbons in using the hosted email flow and uses actual API requests. It checks product journeys, public/session flows, refresh concurrency, CSRF, desktop/mobile layout and WCAG 2.2 AA in both themes. Product screenshots go to the ignored `../.mig/screens/`. Provider fixtures prove application behavior, not compatibility with paid external providers. See the migration progress record for exact runs and limits.

## Deployment

Vercel: project root `web`, use the checked-in Next configuration and the environment above. Register production/preview callback URLs before rollout. Self-hosting: `NEXT_OUTPUT=standalone pnpm build`, copy `public` and `.next/static` into `.next/standalone`, then run `server.js` behind the site's TLS proxy.

`web/vercel.json` selects Next.js and pnpm. The backend bundle contains only the Rust service. Result-file downloads use a short-lived, single-use ticket URL from the backend, avoiding platform upload/download body limits.

## Design and provenance

Shared styles and Arc components are under `styles/`, `components/arc/` and `components/foundation/`. Product components are under `components/mcport/`. See `DESIGN.md`, `vendor/uiarc/PROVENANCE.md`, `vendor/uiarc/LICENSE` and the BDO Grotesk font licenses.
