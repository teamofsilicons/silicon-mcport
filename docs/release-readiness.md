# Release readiness — observed 2026-10-04

This records verification of implementation commit `1f334d9d45f24a89bd4f4775a02d8e1d80f7c2ce` and the remaining release steps. Honeycomb registration, release upload, consent, publication and production deployment have not been performed.

## Current external state

| Check | Observed result |
|---|---|
| Checkout | Reviewed source is committed and pushed through `1f334d9d45f24a89bd4f4775a02d8e1d80f7c2ce`; `origin` is HTTPS. The current development, backend and six-platform runs all target that exact revision. |
| GitHub repository | `teamofsilicons/silicon-mcport` exists with visibility `PRIVATE`; the reviewed source has been pushed. |
| GitHub identity/capability | `saket1225` is the active CLI identity; organization membership is active/admin, and GraphQL reports `viewerCanCreateRepositories: true`. Existing token scopes include `repo` and `workflow`. No token values were printed. |
| Organization Actions policy | Reading the Actions policy returned 403 because the token lacks `admin:org`. Repository creation capability is verified; organization Actions policy/budget is not. This does not establish that running Actions is blocked. |
| Honeycomb CLI/context | Installed `honeycomb 0.6.1`; selected backend is `https://backend.honeycomb.teamofsilicons.com`, home `/Users/codanium`, production context. |
| Live Honeycomb authority | `login status`, `apps get mcport`, `apps organization tos` and `iam` returned HTTP 503 `integration_unavailable` from IAM. A fresh `honeycomb login status --json` retry after the current implementation returned the same error. Current org-admin authority and global `mcport` handle availability are **unverified**. |
| Direct IAM CLI | Default production profile is not signed in; `iam config profiles --json` reports no configured profiles. This is separate from Honeycomb's saved application session. |
| Development checks | [Run 37162702968](https://github.com/teamofsilicons/silicon-mcport/actions/runs/37162702968) passed at `1f334d9`: 82 Rust tests, strict Clippy/formatting, 24 web tests/build, 16 packaging tests, 3 fixture tests and 64 actual-binary E2E assertions. |
| Backend candidate | [Run 37162713767](https://github.com/teamofsilicons/silicon-mcport/actions/runs/37162713767) passed at `1f334d9`: native AL2023 ARM64 tests/build, exact-revision startup/shutdown smoke and website assembly. Downloaded archive SHA-256 `b136ca4082f0fe2f44e744a4828755c7796706f4cab377dd6af56dc774e9d8a4`, all 14 file checksums, build provenance and read-only installer validation passed (`apply: false`). Receipt: `.local/backend-candidate-1f334d9/verification.json`. It has not been deployed. |
| Six-platform candidate | [Run 37162712569](https://github.com/teamofsilicons/silicon-mcport/actions/runs/37162712569) passed all six native jobs and final assembly at `1f334d9`. The downloaded archive passed its checksum and installed official Honeycomb 0.6.1 validation (`valid: true`, no errors). Fresh Honeycomb installation and public release remain unverified. |

Do not recreate or overwrite an existing app based on the Honeycomb 503. Once that integration works, read `honeycomb apps get mcport --json` and the owning organization's apps before deciding whether registration or a revision update is needed. GitHub organization administration does not prove IAM organization administration.

## Reproducing the six native CI results

The workflow uses native Ubuntu 24.04 x86-64/ARM64, macOS 15 Intel/ARM64 and Windows x86-64/ARM64 runners. These labels are listed for private repositories in [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). Linux and Windows ARM64 standard runners have supported private repositories since [January 29, 2026](https://github.blog/changelog/2026-01-29-arm64-standard-runners-are-now-available-in-private-repositories/); no larger-runner workaround is required. The workflow uses SHA-pinned actions and creates artifacts with read-only repository permissions.

For a subsequent candidate, push the reviewed revision and dispatch the native candidate workflow in the existing private repository:

```sh
git push -u origin main
gh workflow run release.yml --repo teamofsilicons/silicon-mcport --ref main
gh run list --repo teamofsilicons/silicon-mcport --workflow release.yml --branch main \
  --json databaseId,headSha,status,conclusion
gh run watch <run-id> --repo teamofsilicons/silicon-mcport --exit-status
gh run download <run-id> --repo teamofsilicons/silicon-mcport \
  --name mcport-honeycomb-candidate --dir .local/release-candidate
```

Repository public visibility is a separate source-publication decision. The native workflow dispatch does not tag, upload to Honeycomb, publish crates, or deploy a backend.

Each native job tests and executes the produced CLI before staging it. Assembly requires all six archives, checks binary CPU/format, hashes and permissions again, and emits `mcport-honeycomb.tar.gz`. The final job installs the published official `silicon-honeycomb-cli` version 0.6.1 and runs `honeycomb validate` before uploading the candidate. Older Linux/glibc compatibility is not established by Ubuntu 24.04 runs.

## Correct Honeycomb artifact contract

[Honeycomb's package reference](https://docs.honeycomb.teamofsilicons.com/package-format/) requires **gzip tar**, with only root `honeycomb.yaml` and `targets/` payloads. The installed CLI confirms `honeycomb validate <directory-or-tar.gz>` and `honeycomb pack <directory> --output <tar.gz>`.

The existing MCPort manifest fields match the contract: `format_version: 1`, optional identity `app_id: mcport`, version `0.1.0`, command mapping `mcport: main`, and six canonical Honeycomb target names, each with its own executable path. The daemon is embedded in the CLI binary.

Native handoff ZIPs are intermediate artifacts only. Final packaging uses deterministic tar/gzip, puts provenance outside the archive, and enforces Honeycomb's 512 MiB compressed / 2 GiB expanded limits. The downloaded current-revision archive has SHA-256 `c02422bfaf2b243404433b95702f3eb8937aabb28c40627e88d81f740d4b5752`, matching its supplied checksum; official Honeycomb 0.6.1 accepted all six targets. Its macOS ARM64 executable exactly matches the CI binary manually tested from a fresh home. Receipt: `.local/release-candidate-1f334d9/proof.json`; see [manual verification](testing/manual.md).

To repeat the downloaded candidate checks:

```sh
cd .local/release-candidate
shasum -a 256 -c SHA256SUMS
honeycomb validate mcport-honeycomb.tar.gz --json
```

## Registration and configuration prerequisites

The current [application configuration reference](https://docs.honeycomb.teamofsilicons.com/application-config/) requires a real IAM org in which the current caller is owner/admin, globally unique app ID, display name (at most 100 bytes), description (50–1,000 words), HTTPS webhook URL, and a separate signing secret of at least 32 characters. `tos` is used by fixtures and is the intended context to verify; live ownership was not established by this audit.

Before constructing protected `application.json`, choose and deploy actual HTTPS backend/website origins and a recognizable logo. Set `/webhooks/iam` to the deployed receiver and configure its matching signing material. Identity and membership disclosure are needed by current login checks; request only those IAM scopes that the deployed implementation uses. MCPort declares no implemented OBO/ATA receiver catalogs, so keep `external`, `obo_endpoints` and `ata_endpoints` empty unless real handlers are added. Provider MCP OAuth is distinct from IAM OBO.

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
honeycomb install 'mcport@0.1.0'
mcport iam --json
mcport login '<fresh MCPort-app-bound SLT>'
mcport login status --json
# Configure a real provider and perform discovery plus a useful authorized tool call.
```

Complete public approval separately with `publication request mcport --revision <app-revision> --message <justification>`, inspect `publication get mcport` / `publication sent`, and respond to reviewers. `publication activate <request-id> --revision <request-revision>` applies only when that request is approved and the caller is authorized. Do not replace a version's bytes or assume app/config/request revisions are interchangeable. See [upload](https://docs.honeycomb.teamofsilicons.com/upload-an-app/) and [publication](https://docs.honeycomb.teamofsilicons.com/publication/).

## Requirement audit of CLI and client

Gateway CRUD, IAM SLT sessions, tools/resources/prompts/completions, tool policy/access, separate provider grants, hosts, activity/cancellation, authorized assets, reports, testing and telemetry settings have public Rust client methods and CLI paths. `mcport-client` exposes the stateless HTTP API and optional local helpers with explicit caller-supplied paths; the CLI uses that facade and owns home/session selection. Daemon polling/progress/results use the shared `mcport-api` transport.

Bundled offline documentation is available as `mcport docs [usage|development]`, including stable JSON topic/content output. Command help includes purposes, input/flag descriptions, examples and related commands. Pending-call recovery names the real `mcport activity show <call-id>` command.

Daemon completion events do reach Space Station through `execution::finish → record_execution` with source `daemon`; this preserves the invocation's opted-out flag and current environment/generation checks. Its local tracing logs alone were not that telemetry path. Actual production Space Station delivery requires configured table keys and live verification.

The explicit organization/access-key/API-key contexts remain unresolved: current IAM introspection accepts Carbon/Silicon application sessions selecting an organization, and rejects other actor types. The user's API-key product decision is pending; do not claim these contexts are implemented merely because generic bearer headers exist.

The candidate CLI's default backend remains loopback unless configured with `MCPORT_URL`, `--backend`, or `config set backend`. A fresh public installation therefore needs either a deployed default endpoint or documented initial endpoint configuration before `iam` works. Published repository/docs/crates links and production Postmark delivery are not proven by local tests. The authoritative [ready-application guide](https://docs.honeycomb.teamofsilicons.com/guides/team-of-silicons-ready-applications/) requires the real discovery → installation → first useful command journey, both identity types and approval completion; those external release gates remain open.
