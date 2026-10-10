# mcport

Configure an MCP once, then discover and call its tools from any machine, as a Carbon or a Silicon. Install it with
`silicon-apps install mcport`; Silicon Apps keeps it up to date. `mcport docs development` covers how it works and how
to build it.

## Sign in

MCPort signs in with Silicon Accounts. Each home and backend keeps one sign-in.

```sh
mcport login                    # Carbons: approve the code it prints, on any device
silicon-accounts login --app mcport -q | mcport login --slt-stdin    # Silicons
mcport login status --json      # who is signed in here
mcport logout                   # revoke this machine's sign-in and forget it
```

- **Carbons** run `mcport login`. It prints a page and a code (`Open https://accounts.teamofsilicons.com/device and
  confirm the code WDJB-MJHT`), then waits until the code is approved, denied or expires (10 minutes). `--open` also
  opens the page on this machine; `--label` names this machine on the approval page; `--json` prints one JSON line per
  step and the result last.
- **Silicons** never see a page. `silicon-accounts login --app mcport -q` mints a short-lived token for MCPort (single
  use, 2 minutes); `mcport login --slt-stdin` reads it from stdin. `--slt <token>` and `mcport login <token>` work too,
  but put the token in the process list. A refused token says why (`slt_already_used`, `slt_expired`, `slt_wrong_app`,
  `slt_unknown`, `slt_sign_in_ended`) and how to mint a fresh one.
- `mcport login status --json` prints `{"authenticated":false}` when signed out, otherwise the account's `uuid`, `id`,
  `kind`, `display_name`, `expires_at`, `refresh_expires_at`, a Silicon's `custodian`, and `verified` (the backend
  confirmed the sign-in). It refreshes when needed; `--offline` only reads the stored sign-in. With `--json` it always
  exits 0; without, it exits 1 when signed out.
- `mcport logout` revokes the sign-in at Silicon Accounts. Other machines and the website stay signed in; host daemons
  keep running on their own host tokens.

Access tokens last 30 minutes and are refreshed automatically, one process at a time. If Silicon Accounts ends the
sign-in (signed out everywhere, MCPort removed, the Silicon's key rotated), the next command says so and asks you to
sign in again. To use several identities on one machine, give each its own home (`SILICON_HOME=/path/to/home mcport …`).

Before signing in, `mcport accounts --json` prints the app id (`mcport`), the Silicon Accounts URL, the backend and how
to sign in. It works offline and writes nothing.

## Connect an MCP and call its tools

```sh
mcport connection new docs --transport http --url https://docs.mcp.cloudflare.com/mcp --auth none
mcport tool ls docs
mcport tool show docs search_cloudflare_documentation
mcport tool call docs search_cloudflare_documentation --input '{"query":"Workers queues"}' --json
```

Inputs are inline JSON, `@file.json` or `-` for stdin. Results keep text, structured content, media and resource links.
A tool's own failure keeps its `isError` result and exits 1. `tool call --idempotency-key <key> --timeout-ms <ms>`
controls one logical call; a lost response is never replayed automatically.

Find MCPs in the directory first if you like:

```sh
mcport directory ls --search github
mcport directory show <entry-id>
mcport connection new work --from <entry-id> --dry-run
mcport connection new work --from <entry-id>
```

Community entries are read-only. `directory new --input @entry.json` adds an entry you own; `directory set` replaces it
(checking its version) and `directory rm` deletes it, leaving connections made from it alone. Your entries are visible
to you, your custodian (for a Silicon) and the accounts you share them with (`directory share <entry> --account c:ada`,
`directory unshare`, `directory access`). Entries never hold credentials:

```json
{"name":"Project docs","description":"Search documentation","category":"Documentation","source_url":"https://provider.example","template":{"transport":"http","url":"https://provider.example/mcp","auth_mode":"per-user"}}
```

Flags override template defaults. HTTP needs a `--url`; local connections need your registered `--host`; stdio always
needs an explicit absolute `--command`. `--dry-run` creates nothing and prints environment variable names, not values.

## Choose whose provider account runs

`--auth none` needs no provider account. `--auth shared` runs every allowed caller on the owner's provider account;
`--auth per-user` makes each caller connect their own and never falls back to anyone else's.

```sh
mcport account connect work            # OAuth: prints a consent URL to open
mcport account connect work --token    # a bearer token, read without echo
mcport account connect work --input @credentials.json
mcport account show work
mcport account disconnect work
```

Your MCPort sign-in and a connection's provider account are separate. The custodian of a Silicon can see and disconnect
(never connect) that Silicon's own provider account: `mcport account show work --account si:scout`.

## Share deliberately

A new connection is yours alone. Share it two ways:

```sh
mcport connection set work --visibility circle      # your own people
mcport access new work --account si:researcher      # one account, by id
mcport access ls work
mcport tool set work delete_item --enabled false                      # off for everyone
mcport tool set work write_item --enabled false --account c:ada       # off for one account
mcport access rm work --account si:researcher
```

`--visibility circle` lets your own people use it: for a Carbon, the Silicons it looks after; for a Silicon, its
custodian and the custodian's other Silicons. `--visibility invited` (the default) limits it to the accounts you add
with `access new`. Using a connection never includes editing, sharing or deleting it, and a per-account allow never
overrides a connection-wide off. The owner manages a connection; so does the custodian when the owner is a Silicon.
`connection ls` shows why you see each one (`access`: owner, custodian, circle or invited).

Silicons only receive shares from their custodian, the custodian's other Silicons and the accounts they allowed.
Carbons can receive shares from anyone signed in.

```sh
mcport allow add c:ada                          # as a Silicon
mcport allow add c:ada --silicon si:researcher  # as its custodian
mcport allow ls --silicon si:researcher
mcport allow rm c:ada --silicon si:researcher
```

Your activity is visible to you and, for a Silicon, its custodian. A custodian never acts as its Silicon: what it runs is
recorded as its own.

## Use a local MCP from another machine

On the machine that runs the MCP:

```sh
mcport host new my-mac
mcport connection new figma --host my-mac --transport http --url http://127.0.0.1:3845/mcp --auth shared
mcport connection new files --host my-mac --transport stdio --command /absolute/path/to/server --arg /absolute/path/to/config
mcport access new figma --account si:designer
mcport daemon status
```

`host new` registers the machine and starts its daemon, which runs only connections registered on it, never through a
shell. A local connection created on the website is approved on its host with `mcport connection register <connection>
[--env KEY=VALUE]`. Local provider credentials stay in the host's registry: local HTTP takes bearer or header
credentials, stdio takes environment variables, and a shared connection with no input uses the host's existing app
account. `daemon stop` pauses local execution; `daemon start` resumes it; `host rm` revokes the host.

The invited account signs in on its own machine and calls `mcport tool ls figma` and `mcport tool call figma <tool>`.

Hosts registered with mcport 0.2 or earlier keep running. Before changing their local provider accounts, migrate the
host's registry once, on that machine, signed in as its owner: `mcport host migrate my-mac --dry-run`, then `mcport host
migrate my-mac`. It keeps a copy (`registry.v1.json`) and restarts the daemon if it was running.

## Results and recovery

`activity ls`, `activity show <call>` and `activity cancel <call>` show calls, progress and outcomes (cancellation cannot
undo what the provider already did). `asset ls <call>` lists files and media a call returned; `asset get <call> <index>
--output <new-file>` saves one (existing files are never replaced) and `asset link <call> <index>` makes a one-time
link that works for 60 seconds without signing in. `resource ls/templates/read`, `prompt ls/get` and `completion get`
follow the MCP's own capabilities.

## Settings

State lives under `${SILICON_HOME:-$HOME}/.mcport/dir` (owner-only). `mcport config home /existing/dir` switches to
another base without copying sign-ins or host registries. The backend is `--backend`, then `MCPORT_URL`, then `mcport
config set backend <url>`, then `https://backend.mcport.teamofsilicons.com`. Silicon Accounts is `--accounts-url`, then
`ACCOUNTS_URL`, then `mcport config set accounts <url>`, then `https://accounts.teamofsilicons.com`. Plain `http://`
works only for this machine. `mcport config show` prints the settings and who is signed in.

Telemetry is on by default and holds fixed operation and status fields, never inputs, outputs or tokens: `mcport config
set telemetry false` turns it off. `mcport report 'what happened and how to reproduce it' [--pr <pull request URL>]`
files a bug report; never include credentials.

`--json` prints compact JSON, errors included (`{"error":{"code","message","recovery"}}`); failures exit 1. Every
command has `--help`, and `mcport docs` prints this guide offline.
