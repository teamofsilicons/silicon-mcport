# mcport-daemon

The outbound execution host connector, embedded in the `mcport` CLI. The public `mcport-client` local feature exposes registry/account helpers and explicit run/start/stop/status operations. The CLI supplies its protected home/session storage and executable path without requiring a system service.

Gateway poll, progress and result calls use the stateless `mcport-api` transport (also re-exported by the public `mcport-client` package) with explicit `HostContext` credentials. No ambient user session or production fallback is used. The daemon retains polling/backoff, cancellation, account isolation and the durable job journal; the client does not retry requests. Result acknowledgements may be retried by job ID, while provider operations are never replayed after an unknown outcome.

Only endpoints registered in the local allowlist execute. Gateway jobs carry IDs and MCP params, never executable paths, URLs or provider credentials. Each job is checked against host organization, current registered account, deadline and concurrency limits. A restart marks unfinished work outcome-unknown. Local account changes cancel existing work and invalidate pooled provider sessions.

MCP HTTP/stdio sessions and bounded local result assets stay in `mcport-mcp` and daemon runtime code. This is distinct from the gateway API transport.

Run `cargo test -p mcport-daemon` for outbound HTTP execution, duplicate-lease/restart protection, local credential isolation and asset materialization tests. The workspace `tests/e2e/run.py` also invokes local HTTP and stdio MCPs from a separate authorized caller home.
