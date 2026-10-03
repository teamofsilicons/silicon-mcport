# UNDERSTANDING.md - Silicon MCPort

Silicon MCPort is our common place to configure MCPs and use them through the CLI. Both Carbons and Silicons should be able to add an MCP, keep it private or share it, and use it from another machine.

Each configured MCP is a connection. Who can use it, where it runs, and whose account it uses are separate things.

# Login

Login and signup are handled by Silicon IAM using the official client. Carbons and Silicons have the same capabilities under the same permissions.

The CLI's `iam` command works before login. Login accepts an app-bound SLT; the app secret stays on the backend. The website has separate Carbon and Silicon login popups and verifies the selected identity type.

Each session belongs to one account and organization. Switching context selects the corresponding session; credentials, caches and pending work stay separate.

# Configuring MCPs

Anyone in an organization can create a connection and becomes its owner. Creating one does not require being an org admin.

A connection has a name, description, organization, owner and configuration. It can use cloud HTTP, local HTTP like Figma desktop, or a local stdio process with its command, arguments and environment settings.

The same MCP can have multiple connections with different accounts, settings and access. Each connection has a stable ID.

There are three ways to configure access to the MCP itself:

- No authentication: authorized users can use the connection directly.
- Per-user configuration: each Carbon or Silicon supplies their own account and required settings.
- Shared configuration: someone configures and authenticates an account, then allows others to use that account through the connection.

I can authenticate as myself and let a Silicon on another server act through my account without receiving my credentials. Show whose account is used. Missing personal authentication never falls back to someone else's account.

Provider authentication is separate from IAM login. Support authorization, refresh and disconnection, and protect credentials at their execution host.

# Sharing and Access

A connection can be private to its owner, available organization-wide, or shared with selected Carbons and Silicons through invitations. all

Permission to use a connection does not grant permission to edit, share or delete it. Public MCPs without authentication still follow our access checks.

All tools are enabled by default, including newly discovered tools. The owner can disable tools connection-wide or per user. Disabled tools stay disabled across refreshes. Per-user settings cannot override connection-wide restrictions. Upstream account permissions still apply.

Check access on every action. Revocation and membership changes block subsequent calls despite cached discovery. Cancel affected pending work where possible; cancellation cannot undo completed actions.

# Local MCPs

A local MCP stays on its machine. Our daemon connects outward to the backend and receives authorized requests. Callers need neither the same machine nor network, and no public local port is needed.

For example: a Silicon on a server calls the CLI, our backend checks access, the daemon on my computer calls my Figma MCP, and the result goes back to that Silicon.

The daemon only invokes registered endpoints and processes. Its host is trusted with the credentials it uses. Sharing exposes the MCP's configured capabilities, including its permitted files and application account.

If the host or MCP is unavailable, show it as offline. Never silently switch machines or accounts. Remote callers need an authorized way to retrieve returned local files and assets.

# Using the Application

The website and CLI list connections I own, organization connections and those shared with me. Show their account, location, tools and status. Both support configuration, authentication, invitations, tool toggles and activity.

The CLI discovers tools, explains inputs and executes them using JSON directly, from a file or stdin. Support available resources and prompts. Preserve structured results, text, media and files. Explain unsupported capabilities and how to continue when input or authorization is needed.

Provide progress, cancellation, clear errors and stable JSON output. Record caller, connection, upstream account, action, time and outcome. Protect activity and results; exclude secrets from logs and exports.

A timeout can mean an unknown outcome. Never automatically repeat a possibly completed action without provider-supported safe retries.

# Honeycomb

Follow the [Team of Silicons-ready application guide](https://docs.honeycomb.teamofsilicons.com/guides/team-of-silicons-ready-applications/). Honeycomb manages registration, configuration, releases and testing lifecycle; IAM handles identity and consent; this app owns connections and access.

Request only needed scopes. Verify signed webhooks, handle duplicate and older events, and refresh authorization. OBO and ATA need declared endpoints, dependencies and appropriate consent or verification. They do not replace connection permissions or provider authentication.

Testing isolates identities, connections, credentials, runners and data. Validate the environment; external MCPs use test accounts or fixtures, never production fallback. Honor Honeycomb's clean, disable, restore and delete lifecycle, prevent stale work from running, and acknowledge completed cleanup.

Ship documented, validated packages for all six Honeycomb native targets. Verify fresh installation, both identity types, both account modes, remote local-MCP use, restrictions, revocation and test isolation. Complete publication approvals and prove the first useful command works.

---

Above is the application behaviour the backend must enforce. Below are its clients: the Rust package, CLI, local daemon and configuration website.

# Rust Package & CLI

The Rust package is the primary interface and is stateless. The CLI is stateful and uses only the package; it has no features missing from the package. The local daemon also uses the package and stays running while serving local MCPs. Build the CLI first; the website is a subset of it.

Expose all public client actions, including read, write, update and delete, under the caller's permissions. Support Carbons, Silicons and authorized organization, access-key and API-key contexts. Backend internal operations are not client commands.

Use Rust, with other runtimes underneath where needed. For results needing a visual interface, return an authorized link to view or download them.

Store local state at `{home_dir}/.mcport/dir`. The default home is `SILICON_HOME`, or `~` when absent. `mcport config home <location>` explicitly changes the home for that CLI context and rejects a location that is not a directory. Keep accounts and test environments separate even when sharing a daemon. Include `ISI` in request metadata when present; nothing should depend on it being set.

# CLI Experience

Application login is exactly `mcport login <slt>`. The caller obtains this app-bound token from the official IAM CLI or IAM website. MCPort's CLI and package do not collect IAM credentials or start their own IAM sign-in ceremony. Upstream MCP authentication is a separate operation and may require the provider's consent flow.

Required commands:

- `mcport iam --json` returns `app_id` and IAM discovery details before login.
- `mcport login status --json` returns `authenticated: true` after successful login, with the Carbon or Silicon and organization.
- `mcport --help` and `mcport -h` show the command tree. Every branch and command has its own help.

Use `mcport <service> <verb> [target] [--flags]` for normal operations, with the login, discovery, configuration and report commands described here. Bundle documentation inside the CLI. Explain each command's purpose, common workflows, inputs, flags and related commands. Errors should say exactly what failed, why, and how to recover. Include the actual GitHub repository, online docs and Rust package links.

Both package and CLI support testing. `mcport --test <test_id> <command>` runs the same operation in the selected test environment with its own validated credentials. A test ID does not grant access. Test-only commands without `--test` must say that the action is only possible in a test environment.

# CLI Examples

These are the intended commands, not a claim that the CLI is already implemented. Connection names resolve within the selected organization; use the stable connection ID if a name is ambiguous. URLs, tool names and inputs below are examples. Discover the actual tool and its schema before calling it.

## Install and Login

```sh
honeycomb install 'mcport'
mcport config home "/existing/silicon/home"
mcport iam --json
mcport login "<app-bound-slt>"
mcport login status --json
mcport connection --help
```

Choose the home before logging in when setting up a new Silicon. Changing home selects that home's stored state; it does not copy another account's credentials.

## Add and Use a Cloud MCP

Create a private connection to a cloud MCP that needs no provider authentication:

```sh
mcport connection new docs --transport http \
  --url "https://mcp.example.com/mcp" --auth none --visibility private

mcport connection ls --json
mcport connection show docs --json
mcport tool ls docs --json
mcport tool show docs search --json
mcport tool call docs search --input '{"query":"release notes"}' --json
```

The saved connection supplies the endpoint and authentication. Each call only needs the connection, tool and that tool's input. For nested or larger input, `query.json` can contain the same JSON:

```sh
mcport tool call docs search --input @query.json --json
cat query.json | mcport tool call docs search --input - --json
```

## Personal and Shared Accounts

For an organization connection where everyone uses their own provider account:

```sh
mcport connection new workspace --transport http \
  --url "https://provider.example.com/mcp" --auth per-user --visibility org
mcport account connect workspace
```

Each caller runs `account connect` for themselves. For a connection using one deliberately shared account, its owner configures and authorizes it once, then grants access:

```sh
mcport connection new shared-work --transport http \
  --url "https://provider.example.com/mcp" --auth shared --visibility private
mcport account connect shared-work
mcport connection set shared-work --visibility invited
mcport access new shared-work --principal "si:researcher"
mcport access new shared-work --principal "<carbon-id>"
```

`account connect` completes the upstream provider's required setup, including browser consent or protected secret input. It is separate from `mcport login`. Secrets are not included in exported connection configuration.

## Share a Local MCP with a Remote Silicon

On the machine hosting the MCP, `host new` registers that machine and starts its daemon. This example uses an already enabled Figma desktop MCP and the desktop app's existing authorization, so no separate `account connect` is needed:

```sh
mcport host new my-mac
mcport connection new figma --host my-mac --transport http \
  --url "http://127.0.0.1:3845/mcp" --auth shared --visibility private
mcport connection set figma --visibility invited
mcport access new figma --principal "si:designer"
```

For a local stdio process instead:

```sh
mcport connection new files --host my-mac --transport stdio \
  --command "/path/to/files-mcp" --arg "/path/to/allowed-folder" \
  --auth none --visibility private
```

The invited Silicon logs into MCPort as itself on its own server, then uses the shared connection:

```sh
mcport tool ls figma --json
mcport tool show figma "<tool-name>" --json
mcport tool call figma "<tool-name>" --input @figma-input.json --json
```

The request still runs on `my-mac`. The Silicon does not need Figma installed or the owner's credentials.

## Change Access and Use Testing

```sh
# Disable a tool for everyone, or just one caller. Use true to enable it.
mcport tool set shared-work "<tool-name>" --enabled false
mcport tool set shared-work "<tool-name>" --principal "si:researcher" --enabled false

# Remove access, or make a connection available to the organization.
mcport access rm shared-work --principal "si:researcher"
mcport connection set docs --visibility org

# Test sessions and connections are separate from production.
mcport --test "<test-id>" login "<test-app-bound-slt>"
mcport --test "<test-id>" connection ls --json
mcport --test "<test-id>" tool call docs search --input @query.json --json
```

The last command requires a separately configured `docs` connection in that test environment.

# Bug Reports

`mcport report "<report-message>" [--pr "<pr-link>"]` submits a bug report. Reports without a PR are valid; show the repository link and invite the caller to reproduce, patch and submit a PR. MCPort is open source, so provide enough information for Carbons and Silicons to investigate and contribute.

For each submitted report, the backend uses Postmark to email `saketdev12@gmail.com`, `shubhastro2@gmails.com` and `bugs@teamofsilicons.com`. Include the report and optional PR without attaching credentials or unrelated private data.

# Docs

Have both usage and development documentation for the Rust package and CLI. Lead with direct instructions: install, log in, configure an MCP and run a tool. Explain how a Carbon can ask their Silicon to do the same. Link to deeper explanations of behaviour, requirements and integration contracts.

Design for Silicons as well as Carbons: make required inputs, permissions, failures and reasons explicit so callers can build on MCPort and recover without guessing.

# Telemetry

Use [Space Station](https://spacestation.teamofsilicons.com/docs) for telemetry from the backend, daemon, CLI and website. Telemetry is on by default and can be turned off in settings, including through the CLI with `mcport config set telemetry false`.

Use its Rust package where applicable. Send useful, self-contained events with source, operation, step, progress, outcome and correlation context. Reuse automatically supplied system metadata. Web analytics and explicit web events may use separate tables.

Keep secrets and raw MCP inputs, results and file contents out of telemetry. Record enough operational detail to diagnose failures without copying private provider data.
