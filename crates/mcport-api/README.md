# mcport-api

Stateless HTTP transport and wire API for Silicon MCPort. Applications should normally depend on [`mcport-client`](../mcport-client/README.md), which re-exports this API and offers explicit local host/configuration helpers behind its `local` feature.

This lower-level crate lets the local daemon use the gateway API without depending on its own public facade. `Client`, `RequestContext` and `HostContext` hold no implicit session/home state. Callers provide credentials and environment context per operation; no mutation is retried. The transport bounds responses and preserves MCP JSON results.

Directory methods are `directory(context, search)`, `directory_entry(context, id)`, `create_directory_entry`, `update_directory_entry`, and `delete_directory_entry`. Updates use `DirectoryUpdate { input, version }` with the version read from the current entry. The directory contains community references and organization-owned entries, scoped by the explicit request context. Its optional `DirectoryTemplate` is public setup metadata: callers review it, supply missing endpoint/host/executable configuration, then create a separate connection. Directory reads never create connections, register hosts, run commands, or configure credentials. These methods and types are also re-exported by `mcport-client`.

Run `cargo test -p mcport-api` for transport contract tests and the public API example.
