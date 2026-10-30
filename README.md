# Silicon MCPort

Configure MCP servers once, then use their tools from the CLI, the Rust package or the website, as a Carbon or a
Silicon. A connection belongs to the account that created it. Share it with your own people (a Carbon and the Silicons
it looks after; a Silicon, its custodian and the custodian's other Silicons) or with specific Carbons and Silicons by id.
A local MCP can run on your machine while a Silicon you shared it with calls it from another machine.

Connection access, where a call runs and whose provider account it uses are independent. Per-user provider accounts
never fall back to a shared one. Shared credentials stay on the execution host; callers receive results.

Everyone signs in with [Silicon Accounts](https://accounts.teamofsilicons.com), and the CLI ships through
[Silicon Apps](https://apps.teamofsilicons.com) ([developer guides](https://developers.teamofsilicons.com)).

## Use it

```sh
silicon-apps install mcport
mcport login                                                       # Carbons
silicon-accounts login --app mcport -q | mcport login --slt-stdin  # Silicons
mcport connection new docs --transport http --url https://docs.mcp.cloudflare.com/mcp --auth none
mcport tool ls docs
mcport tool call docs search_cloudflare_documentation --input '{"query":"MCP Streamable HTTP transport"}' --json
```

`mcport docs` prints the usage guide offline; [docs/usage.md](docs/usage.md) walks through every workflow.

## Run locally

Requires Rust 1.98+. The service needs a Silicon Accounts deployment with an `mcport` app (its secret, its webhook
secret, `device_flow` and `public_client` on):

```sh
cargo build --locked --workspace
ACCOUNTS_URL=http://localhost:9590 MCPORT_APP_SECRET=<app secret> \
  MCPORT_ACCOUNTS_WEBHOOK_SECRET=<whsec_ secret> target/debug/mcport-server        # 127.0.0.1:4380
export MCPORT_URL=http://127.0.0.1:4380 ACCOUNTS_URL=http://localhost:9590
target/debug/mcport login
```

See [development](docs/development.md) for configuration, tests and releases.

## Documentation

- [Usage and workflows](docs/usage.md)
- [Development and deployment](docs/development.md)
- [Release candidates and Silicon Apps packages](scripts/README.md)
- [Service deployment](deploy/README.md) and [the website on Vercel](deploy/vercel.md)
- [Architecture and trust boundaries](docs/architecture.md)
- [API contract](docs/API.md) and [OpenAPI](docs/openapi.yaml)
- [Rust package](crates/mcport-client/README.md) and [CLI guide](crates/mcport-cli/README.md)
- [Migration to Silicon Accounts and Silicon Apps](docs/migration/progress.md)
- [Earlier releases' evidence](docs/history/README.md)
- [Carbon-owned understanding](understanding/UNDERSTANDING.md)

## Status

0.5.0 runs the Rust backend as a Silicon Cells persistent service, with encrypted
records in the cell's PostgreSQL database and the official Silicon Accounts
client 0.7.0. The public API, website and local host protocol keep their existing
addresses and authentication rules. SQLite remains available for local fixtures
and as the verified source for the production migration.

See [Cells deployment](deploy/cells/README.md) for the image, network boundary,
database migration and cutover checks. The deployment receipt must confirm the
actual running source before treating a candidate as live. Earlier Accounts and
Apps release evidence is in [the 0.3.0 record](docs/migration/release-2026-10-10.md).
