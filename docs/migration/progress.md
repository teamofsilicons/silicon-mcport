# MCPort migration progress (Silicon Accounts + Silicon Apps)

Branch `migrate/accounts-apps-20261010`, based on `origin/main` (`db23952`). Each stage appends a dated section:
what it did, commits, test commands and results, what is left, gotchas. Decisions are in
[decisions.md](decisions.md); the proposed contract changes are in
[understanding-proposal.md](understanding-proposal.md).

## 2026-10-10 — Stage 1 (service): baseline before any change

Environment: Rust 1.98.0, Python 3.14.6, `CARGO_TARGET_DIR=$PWD/target/mig`, 96 GB free.
MCPort stores everything in one AES-GCM-encrypted SQLite file; it has no Postgres, Redis or Docker dependency, so
the brief's Postgres on 5460 is not used by this app.

| Command | Result at `db23952` |
|---|---|
| `cargo test --locked --workspace --no-fail-fast` | pass: 113 tests + 2 doctests (server 56, daemon 12+4+1, mcp 3+9, cli 8+4, client 9, api 7, core 0) |
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `python3 -I tests/e2e/run.py --no-build --run-dir .mig/e2e-baseline/run` (with `target/debug -> mig/debug`) | pass: 91 checks (IAM fixture, Honeycomb lifecycle, real binaries) |

Nothing failed or was skipped at the baseline. The server's 56 tests are: store 13, assets 10 (incl. 4 in
`assets::tests`), oauth 9, lifecycle 8, auth 6, hosts 4, directory 4, operations 3, public_ids 3, connections 2,
state 1, tls 1, health 1 (local status).

The baseline e2e run left a real origin/main database (`.mig/e2e-baseline/run/server/mcport.sqlite` + `master.key`,
snapshotted to `.mig/upgrade-fixture/`). It is used below to prove the upgrade path with the real binaries. Its
production rows: 9 connections, 34 calls, 4 credentials, 6 account epochs, 3 policies, 1 host, 1 report, 1 settings,
all in org `tos` for `c:owner` and `si:researcher`, plus IAM-era sessions/refresh families and one Honeycomb test
world.
