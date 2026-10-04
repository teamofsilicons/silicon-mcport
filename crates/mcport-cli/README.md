# mcport

Configure an MCP once, then discover and call its tools through an authorized local or remote identity.

Fresh profiles use `https://backend.mcport.teamofsilicons.com`. Existing saved backend settings are preserved. To use another deployment, run `mcport config set backend https://your-mcport-backend.example` before discovery or login.

```sh
mcport iam --json
mcport login '<app-bound-slt>'
mcport login status --json
mcport connection new notes --transport http --url https://provider.example/mcp --auth shared --visibility invited
mcport account connect notes
mcport access new notes --principal si:researcher
mcport tool ls notes
mcport tool show notes search
mcport tool call notes search --input '{"query":"release"}' --json
```

An app-bound SLT comes from official IAM. Application login and upstream provider authorization are separate. `account connect` returns an OAuth consent URL; `--token` reads a manual bearer token without echoing it. `--input @credentials.json` supports manual bearer/header grants for remote HTTP MCPs. Shared grants intentionally let authorized invitees act as the saved provider account; per-user grants never fall back to the creator's credentials.

```sh
mcport host new laptop
mcport connection new desktop --host laptop --transport http --url http://127.0.0.1:9000/mcp --auth shared --visibility private
mcport connection new files --host laptop --transport stdio --command /absolute/path/to/server --arg /absolute/path/to/config --auth none
mcport daemon status
mcport daemon stop
mcport daemon start
```

Host creation starts the background daemon. The daemon only executes explicitly registered local connections. For a connection first created through the website, run `mcport connection register <connection> [--env KEY=VALUE]` on its registered host to approve that exact endpoint. Local account secrets stay in the host registry. Local HTTP accepts bearer/header credentials; local stdio accepts environment credentials. The host's existing application account is available only to an explicitly shared connection. A disconnected shared grant stays disconnected until explicitly reconnected.

Inputs accept inline JSON, `@file`, or `-` for stdin. `completion get <connection> --input <params>` sends a full MCP completion params object for prompt or resource-template argument suggestions. Results preserve media, resources and structured content. Use `mcport asset ls <call-id>` then `mcport asset get <call-id> <index> --output new-file` to save authorized result bytes; existing files are never replaced. `activity ls/show/cancel` exposes invocation state and best-effort cancellation. `tool call --idempotency-key <key> --timeout-ms <milliseconds>` controls a logical invocation. Cancellation cannot undo an upstream effect, and unknown outcomes are never silently replayed.

The CLI stores state under `{home}/.mcport/dir`. Home starts at `SILICON_HOME`, otherwise the user's home directory; `mcport config home /existing/location` switches the base without copying sessions or local host grants. Backend precedence is `--backend`, then `MCPORT_URL`, then saved configuration, then the production default. For local development, use `MCPORT_URL=http://127.0.0.1:4380 mcport iam --json` or save that loopback URL in a dedicated development home. Sessions and registries are separated by backend and `--test <environment-id>`. `session ls/use` selects a stored principal/org explicitly. Unix directories use 0700 and credentials use 0600. Concurrent CLI processes serialize refresh rotation.

Telemetry is on by default. `mcport config set telemetry false` disables it. Events contain fixed operation/status fields, never provider inputs, outputs or tokens. `mcport report 'reproduction details' [--pr https://github.com/owner/repo/pull/123]` records a bug report and reports delivery status honestly; testing reports never send live email.

`mcport docs` reads this bundled usage guide offline; `mcport docs development` explains architecture, configuration, testing and release limits. `--json` returns documentation as a stable topic/content object. These commands work without a home directory or login. Every service supports `--help`. `--json` prints complete JSON errors and results; operational errors and MCP `isError` results exit nonzero. For verification, run `cargo test -p mcport-cli` and the [end-to-end fixture regression](../../tests/e2e/README.md).
