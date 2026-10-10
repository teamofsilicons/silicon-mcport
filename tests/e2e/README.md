# MCPort end-to-end tests

Two suites run the real `mcport` CLI, its host daemon and `mcport-server` over real HTTP and stdio MCP transports.
Neither bypasses authentication or writes to the database; the journey only reads non-secret record indexes to
prove that deleting a connection removed every grant, policy and provider account on it.

| Suite | Silicon Accounts | Where it runs |
|---|---|---|
| `run.py` — the regression journey | `accounts_fake.py`, a loopback fake with the real API's shapes and rules | CI (`scripts/check.py`) and any machine |
| `accounts_stack.py` — eight scenarios with real tokens | a local Silicon Accounts stack (its testkit) | by hand: `scripts/e2e-accounts.sh` |

## The regression journey (CI)

```sh
python3 -m unittest discover -s tests/e2e -p 'test_*.py'   # fixture self-tests, including Ed25519 vectors
python3 tests/e2e/run.py [--no-build] [--run-dir NEW_DIR] [--base PORT]
```

`run.py` builds the binaries (unless `--no-build`), starts `accounts_fake.py` and then the MCP fixtures and the service
through `scripts/dev_accounts.py` — the same code that runs MCPort against a real local stack, including registering
the webhook and proving a signed test delivery. It uses four consecutive loopback ports (website slot, service,
providers, Silicon Accounts): a free block by default, `--base` or `MCPORT_E2E_BASE` to choose one. Homes and data
are fresh under the run directory (`result.json` and every log stay there); it stops everything it started.

Identities: a Carbon `c:owner`, the Silicon `si:researcher` in its care, and two unrelated Carbons `c:stranger` and
`c:outsider`. Silicons sign in with a short-lived token (`mcport login --slt-stdin`), Carbons through the device flow,
approved at the fake. Coverage: the community directory and personal entries shared by id; invite-only and circle
visibility; sharing by id; pagination and schemas; inline, file and stdin inputs; mixed media, assets, one-time
download links and no-clobber files; current-policy result reads and cancellation redaction; per-account tool
policies; shared and personal provider accounts, the custodian inspecting and disconnecting its Silicon's; provider
OAuth with PKCE and short-lived refresh rotation; connection deletion cleanup; local HTTP and stdio MCPs through a host
daemon (registry version 2), offline hosts and reconnects; idempotency and lost-response no-replay; cancellation;
proofs refused; and every webhook event MCPort handles: id changes, renames, replayed and forged deliveries, the
Silicon allow list, removed access, STK rotation, refresh-token reuse, a custodian transfer and account deletion;
telemetry opt-out and a report with no mail configured.

`accounts_fake.py` signs access tokens with `ed25519.py` (RFC 8032 in pure Python, so the journey needs only the
standard library; never use it outside tests). Its `/fixture/…` endpoints create accounts, mint short-lived tokens,
approve or deny device codes and make the account changes that send webhook events. For exploring by hand,
`python3 tests/e2e/serve.py [--base 4250]` starts the same stack and prints how to sign a Carbon and a Silicon in.

## Scenarios against a local Silicon Accounts stack

```sh
MCPORT_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \
SILICON_ACCOUNTS_CLI=/path/to/silicon-accounts-cli scripts/e2e-accounts.sh [--keep-running]
```

The stack file names the stack's public and API URLs and mcport's development secret (the Silicon Accounts testkit's
shape: `accounts_public_url`, `accounts_api_url`, `apps.mcport.app_secret`, and `apps.interface.app_secret` for the
proof scenario). `scripts/dev-accounts.sh` (started automatically) runs the providers on 4242 and the service on
4241; `stack/mint.mts` creates fresh identities per run (`mcport-e2e-*-<run>`) through the stack's testkit. The eight
scenarios: a Carbon on the API, a Silicon on the CLI, the device flow, custodian and circle and sharing, webhooks,
proofs from another app, the discovery commands from a freshly packed archive, and restart safety. Results land in
`.local/dev-accounts/e2e/run-<run>/` (`result.json`, `transcript.jsonl`; never tokens), and every sign-in made is
signed out at the end.

Fixture runners remove inherited Postmark and telemetry keys, so no live mail or telemetry leaves a test. These
suites prove integration behaviour with controlled providers and accounts, not compatibility with every live
provider or a production deployment.
