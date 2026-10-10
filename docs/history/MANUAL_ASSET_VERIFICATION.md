# Manual asset verification — October 4, 2026

Executed actual CLI commands against a fresh backend on port 4384, the official-IAM-wire fixture on 4394, and a dedicated desktop asset MCP on 4392. Owner and Silicon used separate `SILICON_HOME` directories and separately minted app-bound SLTs. This verifies the real gateway/daemon/CLI path against controlled fixtures; it is not proof of a physical Figma installation.

Observed flow:

1. Carbon `c:owner` signed in, registered `asset-mac`, and created invited shared connection `asset-desktop` at the registered host's `http://127.0.0.1:4392/mcp` endpoint.
2. The owner connected the fixture provider account locally. CLI confirmed `credentials_uploaded: false`.
3. The owner invited `si:researcher`. That Silicon, using its own session and home directory, discovered `design_asset` and invoked it successfully through the relay.
4. The daemon fetched the exact returned `/assets/card.svg` and `/assets/pixel.png` with the configured host account. It did not follow `/assets/redirect.svg` to `/private/secret.txt`, and it did not fetch `file:///etc/passwd`.
5. `mcport asset ls` listed text, SVG, PNG, and structured JSON from call `c04bcd21-84e6-4342-a2ce-bab368e113b4`. The Silicon downloaded SVG and PNG through authenticated gateway URLs. Both byte comparisons matched the fixture source; output permissions were `0600`.
6. Repeating a download to the same output path failed with `File exists`; existing bytes were preserved.
7. The connection owner, acting as a different caller, could not download the Silicon's result (`not_found`).
8. Removing the invitation caused the Silicon's previously valid asset download to fail (`not_found`). Regranting access restored eligibility.
9. A separate explicit `resources/read` call for `fixture://readme` returned text, which downloaded successfully from its own call's asset index.
10. Disabling `design_asset` connection-wide caused the old tool result's asset download to fail (`access_denied`).

A manual failure was fixed during this pass: sentence-ending periods in MCP text had been included in asset filenames. The parser now removes surrounding sentence punctuation from text URL tokens while exact resource URIs remain unchanged. The repeated manual call copied both SVG and PNG and reported one unavailable asset for the deliberately rejected redirect. A regression test covers the discovered case.

Evidence files are local and ignored by Git under `.local/e2e/assets/`: `tool-result.json`, `byte-proof.json`, `downloaded.svg`, `downloaded.png`, and `readme.txt`.

- SVG: 168 bytes; SHA-256 `264dc84d29ecd45dbf668aeb6b1023c76898a879213ae6be2401143ad49bef00`
- PNG: 70 bytes; SHA-256 `2640059609118b695c139804676372eca75c30d30e5906775902d13e44f5356c`

Additional focused automated checks: five daemon asset tests and five server asset tests passed, including exact-origin/path restrictions, active-content response headers, malformed data, and current actor/org/environment/grant/tool-policy checks.
