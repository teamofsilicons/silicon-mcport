# Silicon MCPort

Configure MCP servers once, then use their tools from the CLI or website. Keep connections private, share with your organization, or invite individual Carbons and Silicons. A local MCP can run on your machine while an authorized Silicon calls it from another machine.

Connection access, execution location, and provider account ownership are independent. Per-user accounts never fall back to a shared account. Shared credentials stay on the execution host; callers receive results.

## Run locally

Requires Rust 1.98+, Node.js 22.12+ and Python 3.11+ for the isolated integration fixtures.

```sh
cargo build --locked --workspace
npm ci --prefix web
npm run dev --prefix web
```

In another terminal, run the real backend against disposable IAM/provider fixtures:

```sh
python3 tests/e2e/serve.py --no-build
```

Open `http://127.0.0.1:4381`. Use Carbon login and the fixture IAM consent page. This environment has no production IAM, Postmark, or provider credentials. Its production-shaped and testing-shaped contexts are both disposable fixtures.

For a CLI session, use a fixture SLT printed by the launcher (or obtain a real app-bound SLT from IAM when using a configured deployment):

```sh
export MCPORT_URL=http://127.0.0.1:4380
target/debug/mcport iam --json
target/debug/mcport login '<app-bound-slt>'
target/debug/mcport connection new docs --transport http \
  --url https://docs.mcp.cloudflare.com/mcp --auth none --visibility private
target/debug/mcport tool ls docs --json
target/debug/mcport tool call docs search_cloudflare_documentation \
  --input '{"query":"MCP Streamable HTTP transport"}' --json
```

The public Cloudflare endpoint is a real service. The fixture launcher only substitutes application IAM and explicitly configured loopback providers.

## Documentation

- [Usage and workflows](docs/usage.md)
- [Development and deployment](docs/development.md)
- [Architecture and trust boundaries](docs/architecture.md)
- [API contract](docs/API.md)
- [Manual test evidence](docs/testing/manual.md)
- [Repeatable E2E fixtures](tests/e2e/README.md)
- [Rust client and explicit local host API](crates/mcport-client/README.md)
- [CLI reference](crates/mcport-cli/README.md)
- [Human-owned understanding](understanding/UNDERSTANDING.md)

The website includes actual MIT-licensed [Arc UI](https://uiarc.dev/) components; attribution and source provenance live in `web/`.

## Distribution status

This checkout has verified development candidates: all six native CLI targets passed CI tests and executable smoke checks, the combined archive passed official Honeycomb validation, and an AL2023 ARM64 backend candidate passed its build and startup/shutdown checks. The CI-produced Mac CLI was manually exercised through all three MCP transports. See [release readiness](docs/release-readiness.md) for exact revisions and evidence.

Honeycomb registration, hosted backend configuration, public repository/docs URLs, registry publication, fresh Honeycomb installation and production approvals remain unverified. The candidates have not been deployed or publicly released.
