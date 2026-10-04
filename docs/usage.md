# Using MCPort

Build the CLI from this checkout with `cargo build -p mcport-cli --release`. Add `target/release` to your PATH, or invoke its `mcport` binary directly. Fresh profiles use `https://backend.mcport.teamofsilicons.com`; existing saved settings are preserved. Backend precedence is `--backend`, then `MCPORT_URL`, then saved configuration, then this default. Use `mcport config set backend https://your-mcport-backend.example` for another deployment, or `MCPORT_URL=http://127.0.0.1:4380` with commands against the local development fixtures.

Read bundled instructions with `mcport docs usage` or `mcport docs development`; both work offline before login.

## Log in

```sh
mcport iam --json
mcport login '<app-bound-slt>'
mcport login status --json
```

Obtain the SLT from Silicon IAM for MCPort and the desired account and organization. MCPort does not collect IAM passwords or API credentials. Provider authentication is a separate step. `mcport session ls` and `mcport session use --help` show how to select another saved account.

State lives beneath `${SILICON_HOME:-~}/.mcport/dir`. To choose an existing home before login, run `mcport config home /existing/directory`. Changing home selects different state; it does not copy credentials.

## Create and call a cloud MCP

```sh
mcport connection new docs --transport http \
  --url https://docs.mcp.cloudflare.com/mcp --auth none --visibility private
mcport tool ls docs --json
mcport tool show docs search_cloudflare_documentation --json
mcport tool call docs search_cloudflare_documentation \
  --input '{"query":"Workers queues"}' --json
mcport tool call docs search_cloudflare_documentation --input @query.json --json
cat query.json | mcport tool call docs search_cloudflare_documentation --input - --json
```

The saved connection supplies endpoint and account. Each call supplies the discovered tool name and JSON matching its input schema. Tool-level failures preserve the original `isError` result and exit nonzero. The result retains text, structured content, media and resource references.

## Choose whose provider account runs

Create authenticated connections with `--auth shared` or `--auth per-user`.

```sh
mcport account connect work
mcport account show work --json
```

OAuth prints a consent URL. Review the account and sharing mode there, authorize the provider, then inspect the account again. MCPort uses an explicit `--client-id`, or advertised client ID metadata documents when MCPort has a configured public HTTPS URL, or dynamic client registration. If the provider supports neither automatic option, supply its registered public client ID. For an existing bearer credential, `mcport account connect work --token` reads it without terminal echo. Structured credentials can come from a protected file with `--input @account.json`; avoid secrets in shell history.

Shared accounts are configured by the connection owner. Per-user accounts are configured separately by each caller. Local accounts must be configured or disconnected on the execution host.

## Share deliberately

```sh
mcport connection set work --visibility invited
mcport access new work --principal 'si:researcher'
mcport tool set work delete_item --enabled false
mcport tool set work write_item --principal 'si:researcher' --enabled false
mcport access rm work --principal 'si:researcher'
```

Use the actual tool/principal IDs. Invited users must be members of the connection's organization. Use permission does not include editing, sharing or deleting. A personal allow never overrides a connection-wide deny. `--visibility org` grants use to current members; `private` limits use to the owner.

## Use a local MCP from another machine

On its host:

```sh
mcport host new my-mac
mcport connection new figma --host my-mac --transport http \
  --url http://127.0.0.1:3845/mcp --auth shared --visibility invited
mcport access new figma --principal 'si:designer'
```

The Figma desktop MCP must already be running. Its existing application account is the shared account. For stdio, use `--transport stdio --command /absolute/executable --arg value`; only explicitly registered commands execute, without a shell. `--env NAME=value` configures the local process; protect secret values from shell history.

When the website creates a local connection, run `mcport connection register <id>` on the registered host to approve its local endpoint/process. The gateway cannot install or start arbitrary commands remotely.

The invited Silicon logs in independently, then runs `mcport tool ls figma` and `mcport tool call figma <tool> --input @input.json`. The daemon must stay online. Inspect it with `mcport daemon status`; each command branch has help.

## Results and recovery

Use `resource ls/read`, `resource templates`, and `prompt ls/get` as advertised by `--help`. `activity ls/show/cancel` exposes call IDs, progress and outcomes. `asset ls` and `asset get --help` retrieve caller-owned embedded results and safely materialized local `/assets/` links. Other resource URIs should be read through the provider's MCP resource API.

For recoverable calls, supply `--idempotency-key <unique-operation-id>`. Reusing it with the identical request returns the accepted outcome; a different request conflicts. A timeout, lost response or restart can leave an unknown outcome. Inspect activity and the provider before starting another action. Cancellation cannot undo completed changes.

`mcport --test <environment-id> ...` uses separate validated IAM credentials, sessions, connections and hosts. A test ID alone grants no authority. Telemetry can be disabled with `mcport config set telemetry false`. Bug reports use `mcport report 'reproduction details' [--pr https://github.com/.../pull/1]`; never include credentials or private MCP content.
