# Release readiness — observed 2026-10-04

The website, AWS backend and installed `mcport` CLI are live. Real cloud, local HTTP and local stdio calls passed, including a genuine IAM Silicon on AWS using a Carbon's local Mac MCP and losing access after revocation. All six Rust packages are published at `0.1.0`. Honeycomb distribution remains private; the remaining provider and publication gates are listed below.

## Current external state

| Check | Observed result |
|---|---|
| Deployed revisions | Frontend and six-platform CLI: `7d96ca6bf2a8025d7b98206df4d29a4394a208c0`. Backend: `66c6e0c3775f46a65827ce0cabc8945cfe0e5b2b`. The later backend fixes do not change published client/CLI behavior. |
| GitHub repository | `teamofsilicons/silicon-mcport` is `PUBLIC`, as requested. Tracked content and prior Git blobs were checked for credentials before changing visibility. |
| GitHub identity/capability | `saket1225` is the active CLI identity; organization membership is active/admin, and GraphQL reports `viewerCanCreateRepositories: true`. Existing token scopes include `repo` and `workflow`. No token values were printed. |
| Honeycomb CLI/context | Installed `honeycomb 0.6.1`; selected backend is `https://backend.honeycomb.teamofsilicons.com`, home `/Users/codanium`, production context. |
| Live Honeycomb authority | `c:saket` is authenticated with `tos:org_owner`. Organization inventory was checked before creating `mcport`; registration is accepted, configuration revision1, IAM revision7, state `active`, visibility `private`. One-time credentials were captured in protected operator storage. |
| Live IAM login | Fresh production CLI and normal website login authenticated `c:saket` in `tos`. Genuine isolated IAM SLTs authenticated Carbon `c:mcportqa` on the Mac and Silicon `si:mcportqa` on AWS. Public login rejects actor IDs; a test session cannot authenticate in production. |
| Development checks | [Run 37184383733](https://github.com/teamofsilicons/silicon-mcport/actions/runs/37184383733) passed at `66c6e0c`, including 72 actual-binary E2E assertions, 49 server tests, 25 packaging tests, formatting and strict Clippy. The local restart suite passed all 13 checks. |
| Backend candidate and deployment | [Run 37184426732](https://github.com/teamofsilicons/silicon-mcport/actions/runs/37184426732) passed native AL2023 ARM64 build/tests and exact-revision startup/shutdown smoke at `66c6e0c`. Archive SHA-256 `eed8878406c8a00f8e3a89e310e6b9b9a958e4d2be82c80dfe90d3d3ad9dd3fc`; all 14 payload hashes, provenance and installer validation passed. Installed through SSM; private/public HTTPS health and real IAM/provider calls passed. |
| Six-platform package | [Run 37178993483](https://github.com/teamofsilicons/silicon-mcport/actions/runs/37178993483) passed all six native jobs and assembly at `7d96ca6`. Official Honeycomb 0.6.1 validation passed. Private release `0.1.0` is installed as the normal `mcport` command; installed macOS ARM64 bytes match CI. Public approval remains pending. |
| Rust publication | `mcport-core`, `mcport-mcp`, `mcport-api`, `mcport-daemon`, `mcport-client` and `mcport-cli` version `0.1.0` are available in the registry and sparse index. Downloaded archives match the audited source and checksums. docs.rs pages remain unverified. |
| Space Station | The initial live query of `tos.mcport` verified 14 events: backend7, CLI2, daemon1, web4. Calls `cIJ`, `oHG`, `0GD` and `CFA` each matched two correlation records. At that check the spool had 14 acknowledged and zero pending; no fresh TLS panic or delivery warning. Test-world and other non-production counts are zero. |
| Recovery | A complete stopped-state backup was uploaded to encrypted, versioned private S3, downloaded and restored into an isolated directory. Database integrity, allocator/replay ledgers and all 25 encrypted records passed using the matching restored key. The restored service was not started. |
| Live testing lifecycle | The dedicated QA world completed Honeycomb soft delete, restore and clean callbacks across Honeycomb, IAM and MCPort. Deleted/cleaned sessions were denied; fresh login after restore recovered the same connections. Cleaning advanced generation1→2 and left zero application rows. Production retained its three connections. The QA world is now recoverably deleted and its local daemon stopped. |
| Automatic upgrade | The `66c6e0c` installer upgraded the running deployment with an existing telemetry Unix socket, without operator cleanup. Its stopped-state backup preserved durable data/runtime/key/spool bytes, and database checks passed. All three final CLI transport calls succeeded; the socket recreated and the spool reached 24 acknowledged/zero pending with no fresh warnings or panics. |

AWS stack `silicon-mcport-production` is complete; ARM64 instance `i-0bf2c2f54fce6cfca` uses Elastic IP `100.57.137.244`, encrypted storage, SSM and no SSH ingress. MCPort binds only to loopback behind Caddy. The [backend](https://backend.mcport.teamofsilicons.com/health) and [Vercel website](https://mcport.teamofsilicons.com) both passed normal TLS verification. DNS added only the two MCPort records; all 102 existing Namecheap records and mail mode were preserved and compared after the change. Vercel API responses are private/no-store; deep links, asset cache headers and missing-asset 404s passed. See [deployment instructions](../deploy/aws/deploy.md).

Live manual proof covers Cloudflare discovery/search, file/stdin input and idempotent replay, verified structured-result download, production browser execution, official filesystem write/read, global-deny precedence and a shared local HTTP account. Final production calls with the installed CLI are `OE7` (cloud HTTP), `aD4` (local HTTP) and `mC1` (local stdio with actual Mac bytes verified); the website completed `CFA` after the telemetry fix. In the genuine isolated IAM world, the AWS Silicon called the Mac's shared account (`2LS`), wrote/read an actual Mac file (`EKP`/`QJM`), then lost both execution and previous-result access when revoked. Local HTTP authentication uses a controlled provider. See [manual verification](testing/manual.md) for evidence and boundaries.

Honeycomb's backend participant registry includes MCPort and its dedicated lifecycle secret. App-owned attachment completed for test world `7157c709-82fc-4ae7-9199-dfc417e1a52e`; its own IAM application secret is installed only in that world's map. The world was subsequently cleaned and recoverably deleted after live lifecycle checks, so the retained configuration entry grants no active access. Production credentials are never a fallback. Credential receipts remain in protected ignored storage. Lifecycle proof: `.local/live-manual/isolated/lifecycle/proof.json`.

Manual compact-ID checks preserved all 9 legacy connections, exercised delete/nonreuse and keyed replay across restart, and confirmed that replay did not repeat the provider action. Protected receipt: `.local/e2e/compact-ids/proof.json`.

Final direct-upgrade proof: `.local/deploy/aws/install-66c6e0c3775f46a65827ce0cabc8945cfe0e5b2b/final-proof.json`. Production transport proof: `.local/live-manual/receipts/final-66c6e0c/proof.json`. The stricter stopped-service check was also verified against an absent unit on the actual AL2023 host (`inactive`, `MainPID=0`) to preserve first-install behavior.

## Reproducing the six native CI results

The workflow uses native Ubuntu 24.04 x86-64/ARM64, macOS 15 Intel/ARM64 and Windows x86-64/ARM64 runners. These labels are listed for private repositories in [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). Linux and Windows ARM64 standard runners have supported private repositories since [January 29, 2026](https://github.blog/changelog/2026-01-29-arm64-standard-runners-are-now-available-in-private-repositories/); no larger-runner workaround is required. The workflow uses SHA-pinned actions and creates artifacts with read-only repository permissions.

For a subsequent candidate, push the reviewed revision and dispatch the native candidate workflow in the existing public repository:

```sh
git push -u origin main
gh workflow run release.yml --repo teamofsilicons/silicon-mcport --ref main
gh run list --repo teamofsilicons/silicon-mcport --workflow release.yml --branch main \
  --json databaseId,headSha,status,conclusion
gh run watch <run-id> --repo teamofsilicons/silicon-mcport --exit-status
gh run download <run-id> --repo teamofsilicons/silicon-mcport \
  --name mcport-honeycomb-candidate --dir .local/release-candidate
```

The repository is public. Native workflow dispatch alone does not tag, upload to Honeycomb, publish crates, or deploy a backend.

Each native job tests and executes the produced CLI before staging it. Assembly requires all six archives, checks binary CPU/format, hashes and permissions again, and emits `mcport-honeycomb.tar.gz`. The final job installs the published official `silicon-honeycomb-cli` version 0.6.1 and runs `honeycomb validate` before uploading the candidate. The initial Linux package requires glibc 2.39. The exact ARM64 binary was rejected by the AL2023/glibc 2.34 loader; use a compatible userland such as Ubuntu 24.04. This limitation applies to the CLI package, not the separately built native AL2023 backend.

## Correct Honeycomb artifact contract

[Honeycomb's package reference](https://docs.honeycomb.teamofsilicons.com/package-format/) requires **gzip tar**, with only root `honeycomb.yaml` and `targets/` payloads. The installed CLI confirms `honeycomb validate <directory-or-tar.gz>` and `honeycomb pack <directory> --output <tar.gz>`.

The existing MCPort manifest fields match the contract: `format_version: 1`, optional identity `app_id: mcport`, version `0.1.0`, command mapping `mcport: main`, and six canonical Honeycomb target names, each with its own executable path. The daemon is embedded in the CLI binary.

Native handoff ZIPs are intermediate artifacts only. Final packaging uses deterministic tar/gzip, puts provenance outside the archive, and enforces Honeycomb's 512 MiB compressed / 2 GiB expanded limits. The uploaded `7d96ca6` archive has SHA-256 `b2c1ba2678f2a22bc3f661534c668e889fd9f4bac35fcaa1cd335261fc53c594`. Its installed macOS ARM64 executable has SHA-256 `a88d08395c0c692361f69b713903cc22bf5b1c5dc0c49a44468746dea93afca1`, exactly matching CI and the manually tested candidate. Receipts: `.local/release-candidate-7d96ca6/proof.json`, `.local/deploy/honeycomb/private-release-upload.json`, `.local/deploy/honeycomb/normal-install.json` and `.local/live-manual/receipts/final-production-proof.json`.

To repeat the downloaded candidate checks:

```sh
cd .local/release-candidate
shasum -a 256 -c SHA256SUMS
honeycomb validate mcport-honeycomb.tar.gz --json
```

## Registration and configuration prerequisites

The current [application configuration reference](https://docs.honeycomb.teamofsilicons.com/application-config/) requires a real IAM org in which the current caller is owner/admin, globally unique app ID, display name (at most 100 bytes), description (50–1,000 words), HTTPS webhook URL, and a separate signing secret of at least 32 characters. Live ownership of `tos` was verified through the authenticated Honeycomb session before registration.

Registered origins are `https://backend.mcport.teamofsilicons.com` and `https://mcport.teamofsilicons.com`; `/webhooks/iam` uses independent signing material. The sole requested IAM scope is `self.identity.read`; authorization-envelope organization bindings are checked on every action. MCPort declares no implemented OBO/ATA receiver catalogs, so keep `external`, `obo_endpoints` and `ata_endpoints` empty unless real handlers are added. Provider MCP OAuth is distinct from IAM OBO.

Use explicit `visibility: private` during validation; the default production preference is public and can automatically start approval after an accepted configuration and valid production upload. The application input supports `logo_url`, `website_url`, `docs_url`, `base_url`, `webhook_scope`, `app_scope` and `testing_idle_days`. Do not invent extra lifecycle/callback fields: configure integration details through their supported contracts. Keep secret values out of this document and the repository.

Installed, verified CLI syntax:

```sh
honeycomb login '<fresh Honeycomb-app-bound SLT>'
honeycomb login status --json
honeycomb apps organization <owning-org> --json
honeycomb apps get mcport --json
honeycomb --idempotency-key <stable-create-key> apps create /protected/application.json
honeycomb apps get mcport --json
# Existing application only; use the freshly read revision:
honeycomb --idempotency-key <stable-update-key> apps update mcport \
  /protected/application.json --revision <current-app-revision>
```

Registration can return one-time application credentials; save those directly to protected operator storage. Do not print them in task output. The app secret belongs only in the backend's runtime configuration. A pending registration/acceptance operation is not success.

Logo/package storage requires Honeycomb's separately approved Briefcase OBO grant. When needed, installed syntax is `apps storage start <org>`, `apps storage status <authorization-id>`, and `apps storage complete <authorization-id> --code-file <protected-file> --state <returned-state>`. This asks for explicit provider consent; a Honeycomb login alone does not grant storage.

After real IAM/backend/client verification and accepted configuration:

```sh
honeycomb --idempotency-key <stable-upload-key> releases upload mcport \
  mcport-honeycomb.tar.gz --channel prod --revision <current-app-revision>
honeycomb releases list mcport --channel prod --include-private --json
honeycomb install mcport --version 0.1.0
mcport iam --json
mcport login '<fresh MCPort-app-bound SLT>'
mcport login status --json
# Configure a real provider and perform discovery plus a useful authorized tool call.
```

Complete public approval separately with `publication request mcport --revision <app-revision> --message <justification>`, inspect `publication get mcport` / `publication sent`, and respond to reviewers. `publication activate <request-id> --revision <request-revision>` applies only when that request is approved and the caller is authorized. Do not replace a version's bytes or assume app/config/request revisions are interchangeable. See [upload](https://docs.honeycomb.teamofsilicons.com/upload-an-app/) and [publication](https://docs.honeycomb.teamofsilicons.com/publication/).

## Requirement audit of CLI and client

Gateway CRUD, IAM SLT sessions, tools/resources/prompts/completions, tool policy/access, separate provider grants, hosts, activity/cancellation, authorized assets, reports, testing and telemetry settings have public Rust client methods and CLI paths. `mcport-client` exposes the stateless HTTP API and optional local helpers with explicit caller-supplied paths; the CLI uses that facade and owns home/session selection. Daemon polling/progress/results use the shared `mcport-api` transport.

Bundled offline documentation is available as `mcport docs [usage|development]`, including stable JSON topic/content output. Command help includes purposes, input/flag descriptions, examples and related commands. Pending-call recovery names the real `mcport activity show <call-id>` command.

Daemon completion events reach Space Station through `execution::finish → record_execution` with source `daemon`; this preserves the invocation's opted-out flag and current environment/generation checks. Production delivery from all four sources was verified at watermark14 after fixing startup's competing Rustls providers. Query proof: `.local/deploy/spacestation/verification/final-3400f4e/proof.json`; runtime/spool proof: `.local/deploy/aws/install-3400f4e2e70f0bfbc26409b8f6e56884c2efbc37/telemetry-recovery.json`. No raw provider arguments, results or credentials were queried or printed.

Application login uses IAM Carbon/Silicon sessions in authorized organization contexts. The user explicitly excluded MCPort API keys. Upstream provider credentials remain separate connection configuration; IAM application-verification keys do not grant user or connection permissions.

The installed CLI defaults to `https://backend.mcport.teamofsilicons.com`; explicit `--backend`, `MCPORT_URL` and saved configuration still override it. Fresh-profile precedence and production discovery passed. MCPort does not issue application API keys.

## Remaining acceptance gates

- Configure and verify production Postmark report delivery with a verified sender. Provider access is pending; no report email has been sent.
- Complete Honeycomb publication approval. The private release is installed and reviewable; no reviewer request has been sent. Rust package publication is complete, while docs.rs rendering remains unverified.
- A useful Figma design read still requires an accessible file and qualifying desktop MCP access. [Figma's current guide](https://help.figma.com/hc/en-us/articles/32132100833559-Guide-to-the-Figma-MCP-server) requires a Dev or Full seat on a paid plan for its desktop server. Six-tool discovery and permission-error forwarding passed; the earlier access error does not identify which requirement is missing, and no successful design read is claimed.
- Live provider OAuth consent remains unverified. Controlled shared and per-user OAuth checks passed, including six successful refresh-token rotations, new-bearer use, account isolation and disconnect rejection. The expanded real-binary suite passed all 72 assertions; fixture tests passed 4/4. Proof: `.local/e2e/oauth-refresh-20261004/journey/oauth-refresh-proof.json`.

All six native packages passed CI execution and validation; manual journeys cover macOS ARM64 and Linux ARM64 with Ubuntu24 userland. Manual installation on every remaining architecture is additional coverage, not a claim supported by these checks. docs.rs rendering is also unverified; usage/development documentation is available in the repository and bundled CLI.
