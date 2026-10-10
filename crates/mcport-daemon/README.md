# mcport-daemon

The outbound host connector, embedded in the `mcport` CLI (`mcport daemon run`, started by `mcport host new` and
`mcport daemon start`). `mcport-client`'s `local` feature exposes the registry helpers and explicit run/start/stop/status
operations; the daemon installs no system service and has no updater (Silicon Apps updates the CLI).

The daemon long-polls the service with its host token through the stateless `mcport-api` client, runs only endpoints
registered in its local registry, and keeps a durable job journal. Jobs carry ids and MCP params, never executable
paths, URLs or provider credentials. Each job is checked against the registered connection, the caller's local provider
account, its deadline and the concurrency limit (four jobs). A restart marks unfinished work outcome-unknown; result
uploads are retried by job id, provider actions never are.

The registry (`registry.json`, mode 0600) holds the host token, registered endpoints and local provider credentials.
Version 2 keys the owner and each caller's personal provider account by Silicon Accounts uuid, using the uuid the
service sends with each job and never a changeable public id. Version 1 registries (mcport 0.2 and earlier) keyed them by
old ids: they still load and run, the service keeps sending the old key for such hosts, and `mcport host migrate`
rewrites them as version 2. The daemon reports `registry_version` with its capabilities.

Local registry changes cancel affected jobs and health probes on the next 250 ms control tick. A gateway poll in flight
is drained because it may already hold a leased job; newly registered connections can therefore wait for the current
20-second poll. Local account changes also invalidate pooled provider sessions, which are kept per connection and
caller.

MCP HTTP and stdio sessions and bounded local result assets live in `mcport-mcp` and the daemon's runtime.

Run `cargo test -p mcport-daemon` for outbound execution, duplicate-lease and restart protection, local credential
isolation, registry versions and asset materialization.
