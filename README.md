# Silicon MCPort

Configure MCP servers once, then use their tools from the CLI or website. Share connections with your organization or invite individual Carbons and Silicons. Invite-only connections are owner-only until someone is invited. A local MCP can run on your machine while an authorized Silicon calls it from another machine.

Connection access, execution location, and provider account ownership are independent. Per-user accounts never fall back to a shared account. Shared credentials stay on the execution host; callers receive results.

## Run locally

Requires Rust 1.98+, Node.js 22.12+ and Python 3.11+ for the isolated integration fixtures.

```sh
cargo build --locked --workspace
npm ci --prefix web
npm run dev --prefix web
```

In another terminal, run the real backend against disposable IAM/provider fixtures:

```sh
python3 tests/e2e/serve.py --no-build
```

Open `http://127.0.0.1:4381`. Use Carbon login and the fixture IAM consent page. This environment has no production IAM, Postmark, or provider credentials. Its production-shaped and testing-shaped contexts are both disposable fixtures.

For a CLI session, use a fixture SLT printed by the launcher (or obtain a real app-bound SLT from IAM when using a configured deployment):

```sh
export MCPORT_URL=http://127.0.0.1:4380
target/debug/mcport iam --json
target/debug/mcport login '<app-bound-slt>'
target/debug/mcport connection new docs --transport http \
  --url https://docs.mcp.cloudflare.com/mcp --auth none
target/debug/mcport tool ls docs --json
target/debug/mcport tool call docs search_cloudflare_documentation \
  --input '{"query":"MCP Streamable HTTP transport"}' --json
```

The public Cloudflare endpoint is a real service. The fixture launcher only substitutes application IAM and explicitly configured loopback providers.

Browse the directory with `mcport directory ls --search github`, inspect an entry with `mcport directory show <id>`, then review its configuration with `mcport connection new work --from <id> --dry-run`. Omit `--dry-run` to create it after supplying any missing endpoint or local host details. Your org can add reusable entries with `mcport directory new --input @entry.json`; provider credentials stay outside the directory. No-auth connections default to org access; authenticated connections default to invite-only. See [directory setup](docs/usage.md#find-or-add-a-directory-entry).

## Documentation

- [Usage and workflows](docs/usage.md)
- [Development and deployment](docs/development.md)
- [Architecture and trust boundaries](docs/architecture.md)
- [API contract](docs/API.md)
- [Manual test evidence](docs/testing/manual.md)
- [Repeatable E2E fixtures](tests/e2e/README.md)
- [Rust client and explicit local host API](crates/mcport-client/README.md)
- [CLI reference](crates/mcport-cli/README.md)
- [Human-owned understanding](understanding/UNDERSTANDING.md)

The website includes actual MIT-licensed [Arc UI](https://uiarc.dev/) components; attribution and source provenance live in `web/`.

## Distribution status

Directory setup and the revised access defaults are development changes in this checkout; they are not part of the deployed `0.1.0` release yet. [Directory verification](docs/testing/directory.md) records the passing checks and pending manual browser pass.

The [website](https://mcport.teamofsilicons.com) and AWS backend are live. All six native CLI targets passed CI tests and executable smoke checks; the combined archive passed official Honeycomb validation and release `0.1.0` is public. Install it with `honeycomb install mcport`. Real cloud, local HTTP and local stdio calls passed, including an IAM Silicon on AWS using a Carbon's local Mac MCP, writing/reading an actual file, and losing access immediately after revocation. An authenticated GitHub MCP also passed through the CLI and website. See [release readiness](docs/release-readiness.md) for exact revisions and evidence.

The six Rust packages are published at `0.1.0`, including the [CLI](https://crates.io/crates/mcport-cli/0.1.0) and [client library](https://crates.io/crates/mcport-client/0.1.0). Registry downloads and source checksums were verified; docs.rs rendering is still pending verification.

The initial Linux CLI binaries require glibc 2.39 (for example, Ubuntu 24.04); they do not run natively on Amazon Linux 2023/glibc 2.34. The native AWS backend is built separately for AL2023 ARM64. macOS and Windows packages are also available in the public six-target archive.
