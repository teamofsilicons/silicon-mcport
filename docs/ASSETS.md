# Result files and local MCP assets

MCPort exposes downloadable content through the invocation that returned it. The server accepts a call ID and asset index; it never accepts a URL or host filesystem path for download.

```sh
mcport asset ls <call-id> --json
mcport asset get <call-id> <index> --output ./result.svg
mcport asset link <call-id> <index>
```

Asset lists include a name, media type, byte length, source URI where present, and an authenticated download URL. The URL contains no bearer token. Results belong to the account that made the call; that account's custodian (for a Silicon) can read them too. Downloads recheck the caller's sign-in, call ownership, current connection access and current tool restrictions. Removing an invitation or disabling the originating tool blocks later downloads, including previously copied links. A new sign-in of the same account can retrieve the result. `asset link` (`POST /api/v1/calls/{id}/assets/{index}/ticket`) makes a one-time URL that works without a token for 60 seconds, re-checking the same access when used, and ends when its holder signs out everywhere.

`GET /api/v1/calls/{id}/assets` returns `ResultAsset[]` in the usual JSON envelope. `GET /api/v1/calls/{id}/assets/{index}` returns raw bytes with attachment disposition, `nosniff`, a sandbox CSP, and no-store caching. Active or unrecognized media types, including SVG and HTML, are served as `application/octet-stream`. The SDK returns bytes; the CLI writes an explicit output file without overwriting an existing path.

Downloadable content includes MCP text, image and audio blocks, embedded resource text/blob content, `resources/read` contents, structured JSON output, and materialized host assets. Credential values are never added to asset metadata. The original provider result remains intact in encrypted result storage.

## Desktop assets

Some local HTTP MCPs, including Figma desktop, return localhost `/assets/…` links. A remote Silicon cannot use that localhost address directly. Immediately after a successful tool call or resource read, the daemon may copy a small returned asset into that call's result:

- The exact URL must have appeared in the just-completed response, including resource links or returned text/code.
- Its scheme, host, and port must match the registered MCP HTTP endpoint's origin. `localhost` and `127.0.0.1` are different origins. Configure the endpoint using the hostname actually emitted by the MCP if materialization is needed.
- Its literal path must begin `/assets/` and contain only ordinary filename segments. Dot traversal, percent escapes, backslashes, query strings, fragments, user information, and other paths are rejected.
- The daemon uses GET, no redirects, no proxy, and only the selected execution account's credentials on that exact origin.
- Limits are 16 candidates, 5 MiB per file, 10 MiB total copied bytes, a 3-second total fetch budget bounded by the invocation deadline, and a 16 MiB final result. Cancellation stops further fetching.

Copies are attached under `result._meta.mcport.assets` as embedded resource objects (`uri`, `mimeType`, `blob`). `unavailable_asset_count` records failed eligible fetches. An unavailable asset never causes a completed tool action to be retried or changes its outcome into a failure. Unsupported origins and paths remain in the original result; they are not fetched.

This does not grant arbitrary access to a host's files. `file://` is never read by MCPort, and stdio connections do not gain an HTTP fetch origin. When an MCP exposes its own resource URI, use `mcport resource read` to ask that MCP for authorized content; the returned embedded text/blob becomes a downloadable asset in that new call. Providers that return only an unsupported private URL must supply a usable resource or an allowed asset endpoint.

## Verification

Daemon tests cover exact origin and literal path checks, selected-account credential scope, no redirects, cancellation, and no filesystem access. Backend tests cover extraction, malformed base64, safe response headers/filenames, download tickets, and current caller, custodian, invitation and tool-policy enforcement. End-to-end release checks should create a local HTTP MCP that returns `/assets/` links, invoke it as an invited Silicon through the relay, download and compare the bytes, then revoke access and confirm the same asset request is denied.

The website exposes Result files below tool/resource results and in Activity call details. Its server holds the sign-in and fetches small results for the browser; larger downloads use one-time tickets, so no token reaches the browser. The browser does not navigate to local provider URLs or execute SVG/HTML. Stable `/activity/<call-id>` links reopen the caller's results, with server permission checks.
