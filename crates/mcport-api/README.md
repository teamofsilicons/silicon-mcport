# mcport-api

Stateless HTTP transport and wire API for Silicon MCPort. Applications should normally depend on [`mcport-client`](../mcport-client/README.md), which re-exports this API and offers explicit local host/configuration helpers behind its `local` feature.

This lower-level crate lets the local daemon use the gateway API without depending on its own public facade. `Client`, `RequestContext` and `HostContext` hold no implicit session/home state. Callers provide credentials and environment context per operation; no mutation is retried. The transport bounds responses and preserves MCP JSON results.

Run `cargo test -p mcport-api` for transport contract tests and the public API example.
