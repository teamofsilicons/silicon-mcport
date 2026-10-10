# Using MCPort

Install the CLI with `silicon-apps install mcport`; Silicon Apps keeps it current. From a checkout, `cargo build -p
mcport-cli --release` builds `target/release/mcport`. The CLI talks to `https://api.mcport.teamofsilicons.com` and
signs in at `https://accounts.teamofsilicons.com` unless told otherwise: the backend is `--backend`, then `MCPORT_URL`,
then `mcport config set backend <url>`; Silicon Accounts is `--accounts-url`, then `ACCOUNTS_URL`, then `mcport config
set accounts <url>`. Plain `http://` is accepted only for this machine.

`mcport docs usage` and `mcport docs development` print the bundled guides offline, before signing in.

## Sign in

```sh
mcport accounts --json          # app id, Silicon Accounts URL, backend; offline
mcport login                    # Carbons: confirm the printed code on any device
silicon-accounts login --app mcport -q | mcport login --slt-stdin    # Silicons
mcport login status --json
mcport logout
```

Carbons use the device flow: `mcport login` prints a page and a code, and finishes when the code is approved there
(denied or unapproved codes fail after at most 10 minutes). `--open` opens the page on this machine and `--json` prints
one JSON line per step. Silicons get a short-lived token for MCPort from Silicon Accounts (single use, 2 minutes) and
hand it over on stdin, or with `--slt <token>`. MCPort never sees a Silicon Accounts password, code or key.

`login status --json` prints `{"authenticated":false}` or the account's `uuid`, `id`, `kind`, `display_name`, expiry
times and, for a Silicon, its `custodian`. `logout` revokes this machine's sign-in at Silicon Accounts; other machines
and the website stay signed in. Sign-ins live beneath `${SILICON_HOME:-~}/.mcport/dir`, one per home and backend; use
separate homes for separate identities. `mcport config home /existing/directory` switches homes without copying
credentials.

Provider authentication is a separate step (below).

## Create and call a cloud MCP

```sh
mcport connection new docs --transport http \
  --url https://docs.mcp.cloudflare.com/mcp --auth none
mcport tool ls docs --json
mcport tool show docs search_cloudflare_documentation --json
mcport tool call docs search_cloudflare_documentation \
  --input '{"query":"Workers queues"}' --json
mcport tool call docs search_cloudflare_documentation --input @query.json --json
cat query.json | mcport tool call docs search_cloudflare_documentation --input - --json
```

The saved connection supplies the endpoint and account; each call supplies the tool name and JSON matching its input
schema. A tool's own failure keeps its `isError` result and exits nonzero. Results keep text, structured content, media
and resource references.

A new connection is visible to its owner only until it is shared (below).

## Find or add a directory entry

```sh
mcport directory ls --search github --json
mcport directory show <entry-id> --json
mcport connection new work --from <entry-id> --dry-run --json
mcport connection new work --from <entry-id>
```

The community list comes from the MIT-licensed [Awesome MCP Servers repository](https://github.com/wong2/awesome-mcp-servers)
associated with [mcpservers.org](https://mcpservers.org/). It is a bundled snapshot with each entry's source and
revision, readable by everyone signed in. Reviewed templates prefill settings; other entries need an endpoint or local
command from their setup instructions. `--from` never installs a package or interprets shell snippets; stdio always
needs an explicit absolute `--command`.

Your own entries are personal: visible to you, your custodian (for a Silicon) and the accounts you share them with.
Save this as `entry.json`:

```json
{
  "name": "project-docs",
  "description": "Our documentation MCP",
  "category": "Documentation",
  "source_url": "https://example.com/setup",
  "template": {"transport": "http", "url": "https://mcp.example.com/mcp", "command": null, "args": [], "auth_mode": "none"}
}
```

```sh
mcport directory new --input @entry.json --json
mcport directory set <entry-id> --input @entry.json --json
mcport directory share <entry-id> --account c:ada
mcport directory access <entry-id>
mcport directory unshare <entry-id> --account c:ada
mcport directory rm <entry-id>
```

Entries are descriptions, not configured accounts: adding one creates no connection and grants nobody's provider
account. Editing or removing one leaves existing connections alone. Credentials belong in `account connect` or the
host's local configuration, never in directory fields. Each account may keep 1000 entries.

## Choose whose provider account runs

Create authenticated connections with `--auth shared` (every allowed caller runs on the owner's provider account) or
`--auth per-user` (each caller connects their own, with no fallback).

```sh
mcport account connect work
mcport account show work --json
```

OAuth prints a consent URL. MCPort uses an explicit `--client-id`, the provider's client ID metadata documents, or
dynamic client registration; supply a registered public client ID only when the provider supports neither automatic
option. `mcport account connect work --token` reads a bearer token without echo; `--input @account.json` reads
structured credentials from a protected file.

The owner (or the custodian of a Silicon owner) configures a shared account; each caller configures their own per-user
account. A custodian can inspect and disconnect a Silicon's own provider account (`--account si:scout`) but never
connect one for it. Local accounts are configured on the execution host.

Another person can finish a provider consent step by opening the returned MCPort authorization link within ten minutes.
The page names the account that started it, and the provider grant stays with that account.

## Share deliberately

```sh
mcport connection set work --visibility circle
mcport access new work --account si:researcher
mcport tool set work delete_item --enabled false
mcport tool set work write_item --account si:researcher --enabled false
mcport access rm work --account si:researcher
```

`--visibility circle` shares a connection with your own people: for a Carbon, the Silicons it looks after; for a
Silicon, its custodian and the custodian's other Silicons. `--visibility invited` (the default) limits it to the owner
and the accounts added with `access new`, by `c:`/`si:` id. Use never includes editing, sharing or deleting, and a
per-account allow never overrides a connection-wide off. The custodian of a Silicon manages that Silicon's connections
like its owner, but anything it runs is recorded as its own, never as the Silicon.

Silicons are not open to everyone: sharing with a Silicon outside your own people needs that Silicon, or its
custodian, to allow you first.

```sh
mcport allow add c:ada --silicon si:researcher     # run by si:researcher's custodian (or by the Silicon, without --silicon)
mcport allow ls --silicon si:researcher
mcport allow rm c:ada --silicon si:researcher
```

Carbons can receive shares from anyone signed in.

## Use a local MCP from another machine

On its host:

```sh
mcport host new my-mac
mcport connection new figma --host my-mac --transport http \
  --url http://127.0.0.1:3845/mcp --auth shared
mcport access new figma --account si:designer
```

The Figma desktop MCP must already be running; its existing app account is the shared account. For stdio, use
`--transport stdio --command /absolute/executable --arg value`; only explicitly registered commands run, without a
shell. `--env NAME=value` configures the local process; keep secret values out of shell history.

When the website creates a local connection, run `mcport connection register <id>` on the registered host to approve its
endpoint or process. The service cannot install or start arbitrary commands remotely.

The invited Silicon signs in on its own machine, then runs `mcport tool ls figma` and `mcport tool call figma <tool>
--input @input.json`. The daemon must stay online: `mcport daemon status`.

Hosts registered with mcport 0.2 or earlier keep serving their connections. Migrate one before changing its local
provider accounts: on the host, signed in as its owner, run `mcport host migrate my-mac --dry-run` and then `mcport host
migrate my-mac`. If some local provider accounts belong to old ids MCPort cannot match to an account, it stops and names
them: start the daemon so it reports them and retry, or pass `--drop-unmapped` to remove those credentials (their
accounts connect again with `account connect`). The original registry is saved as `registry.v1.json`.

## Results and recovery

`resource ls/read`, `resource templates` and `prompt ls/get` follow the MCP's capabilities. `activity ls/show/cancel`
shows call ids, progress and outcomes, for your calls and those of Silicons you look after. `asset ls` and `asset get`
save embedded results (never overwriting a file); `asset link` makes a one-time link that works for 60 seconds.

For recoverable calls, supply `--idempotency-key <unique-operation-id>`: the same request returns the accepted outcome,
a different one conflicts. A timeout, lost response or restart can leave an unknown outcome; inspect activity and the
provider before acting again. Cancellation cannot undo completed changes.

Telemetry is off with `mcport config set telemetry false`. Bug reports use `mcport report 'reproduction details' [--pr
https://github.com/.../pull/1]`; never include credentials or private MCP content.
