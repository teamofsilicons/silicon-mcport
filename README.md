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
- [Architecture and trust boundaries](docs/architecture.md)
- [API contract](docs/API.md) and [OpenAPI](docs/openapi.yaml)
- [Rust package](crates/mcport-client/README.md) and [CLI guide](crates/mcport-cli/README.md)
- [Migration to Silicon Accounts and Silicon Apps](docs/migration/progress.md)
- [Earlier releases' evidence](docs/history/README.md)
- [Carbon-owned understanding](understanding/UNDERSTANDING.md)

## Status

0.3.0 moves MCPort to Silicon Accounts and Silicon Apps: personal accounts with explicit sharing and custodians, the
device flow and short-lived tokens for sign-in, and Silicon Apps packages. It is not released yet; the
[cutover runbook](docs/migration/cutover.md) lists what the operator does to switch production over. Earlier releases
(0.1.x) and their evidence are recorded under [docs/history](docs/history/README.md).
