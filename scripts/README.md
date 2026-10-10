# Development gates and release candidates

## Local gates

```sh
python3 scripts/check.py
```

`check.py` runs Rust formatting, the workspace tests, strict Clippy, the script tests
(`scripts/tests`), the three Silicon Apps discovery commands against the CLI the tests just
built, the web tests and build, and the real-binary end-to-end journey in `tests/e2e`
(against a fake Silicon Accounts; `MCPORT_E2E_BASE` picks its four ports). `--skip-web` and
`--skip-e2e` narrow it; CI runs all of it. It installs nothing: use Python 3.11+ (standard
library only), Node 24+ and the current stable Rust toolchain.

## Running against a local Silicon Accounts stack

```sh
MCPORT_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build]   # start (idempotent)
scripts/e2e-accounts.sh                                                        # eight scenarios, real tokens
scripts/dev-accounts-stop.sh                                                   # stop
```

`dev-accounts.sh` (`dev_accounts.py up`) starts the MCP fixtures on 127.0.0.1:4242 and the
service on 127.0.0.1:4241 against a Silicon Accounts on this machine, registers mcport's
webhook there with the app's credentials and proves a signed test delivery; `restart` and
`status` are the other commands. `e2e-accounts.sh` needs a silicon-accounts checkout
(`SILICON_ACCOUNTS_DIR`, for its testkit, and `SILICON_ACCOUNTS_CLI`); see
[tests/e2e/README.md](../tests/e2e/README.md).

## Silicon Apps packages

MCPort's CLI reaches Carbons and Silicons only through Silicon Apps
(`silicon-apps install mcport`), whose updater keeps installed copies current; the CLI has no
updater of its own. A release is one archive per target:

- `dist/apps/mcport-<version>-<target>.tar.gz`, holding `apps.yaml` (listing only that
  target) and the binary at `bin/mcport` (`bin/mcport.exe` on Windows), and
- `dist/apps/mcport-<version>-<target>.tar.gz.sha256`.

`packaging/apps.yaml.in` is the manifest template. `scripts/package-apps.sh` builds one
archive:

```sh
scripts/package-apps.sh <version> <target> <binary>
```

It refuses, with the reason, a version other than the workspace version in `Cargo.toml`, a
name that is not a Silicon Apps target, and a binary that is not an executable for that
target (Linux binaries must be static, Windows ones console programs). Whenever this
machine can run the binary, it then runs `mcport --version`, `--help`, `accounts --json`
and `login status --json` in an empty home with nothing else in the environment, exactly as
Silicon Apps' validation workers do, and packs nothing unless each answers as required
(`"app_id":"mcport"`, `{"authenticated":false}`, no files written). It runs
`silicon-apps validate` and `silicon-apps pack`, opens the archive to confirm it holds
exactly the manifest and the checked binary, runs the commands again from the extracted
copy, and writes the `.sha256`. It never overwrites an archive.

`PACKAGE_APPS_EMULATOR=qemu-arm` (or `qemu-aarch64`) runs a foreign Linux binary through
qemu-user for those checks. `--check-only [--record FILE]` runs only the binary checks and
writes a record of them; `--checked-record FILE` lets a machine that cannot run a binary
pack it on the strength of that record (same target, version and SHA-256);
`--require-discovery` refuses to pack when neither happened. `--allow-dynamic` (with `--check-only` only) accepts a
dynamically linked Linux development build, which is how `check.py` checks the debug CLI; packages are always static.

The packer is the official Silicon Apps CLI, 0.2.0 or newer:
`cargo install --locked silicon-apps-cli --version 0.2.0`. `validate` and `pack` are local
and need no sign-in; the script runs them with an empty Apps home so no session is read.

On a Mac with Apple silicon:

```sh
cargo build --locked --release -p mcport-cli
scripts/package-apps.sh 0.3.0 macos-aarch64 target/release/mcport
```

A Linux archive from any machine, built static with Zig (on a Mac the script cannot run
the Linux binary, says so and packs it; CI runs the checks on Linux):

```sh
python3 -m pip install 'cargo-zigbuild==0.23.4' 'ziglang==0.15.2'
rustup target add x86_64-unknown-linux-musl
cargo zigbuild --locked --release -p mcport-cli --target x86_64-unknown-linux-musl
scripts/package-apps.sh 0.3.0 linux-x86_64 target/x86_64-unknown-linux-musl/release/mcport
```

## Release workflow

`.github/workflows/release.yml` runs on a `v*` tag (which must equal the CLI version) or
by hand. It has read-only repository permissions and publishes nothing.

| Target | Runner | Rust target | Tests on the runner |
|---|---|---|---|
| `linux-x86_64` | ubuntu-24.04 | `x86_64-unknown-linux-musl` (Zig) | yes (gnu host) |
| `linux-i686` | ubuntu-24.04 | `i686-unknown-linux-musl` (Zig) | no; commands checked natively |
| `linux-aarch64` | ubuntu-24.04-arm | `aarch64-unknown-linux-musl` (Zig) | yes (gnu host) |
| `linux-armv7hf` | ubuntu-24.04 | `armv7-unknown-linux-musleabihf` (Zig) | no; commands checked through qemu-arm |
| `macos-x86_64` | macos-15-intel | `x86_64-apple-darwin` | yes |
| `macos-aarch64` | macos-15 | `aarch64-apple-darwin` | yes |
| `windows-x86_64` | windows-2025 | `x86_64-pc-windows-msvc` | yes |
| `windows-aarch64` | windows-11-arm | `aarch64-pc-windows-msvc` | yes |

Each native job tests the CLI, client, daemon and MCP transports, builds the release CLI
(the local daemon is embedded in `mcport`), and runs `package-apps.sh --check-only` on the
binary. The packaging job installs `silicon-apps-cli` 0.2.0, packs one archive per target
(checking the Linux ones again, natively or through qemu; macOS and Windows ones by their
native records), writes `SHA256SUMS` and uploads the artifact
`mcport-silicon-apps-release` (archives, `.sha256` files, `SHA256SUMS` and the check
records in `checks/`).

Today only the four Linux validation workers are live on Silicon Apps (`linux-x86_64`,
`linux-i686`, `linux-aarch64`, `linux-armv7hf`), so only those archives can be uploaded.
The macOS and Windows archives are built and kept for when their workers go live.

## Publishing (an operator, after review)

Sign in to Silicon Apps as an author of `mcport`, then, from the downloaded artifact:

```sh
sha256sum -c SHA256SUMS
silicon-apps upload mcport --target linux-x86_64 mcport-0.3.0-linux-x86_64.tar.gz
silicon-apps upload mcport --target linux-i686 mcport-0.3.0-linux-i686.tar.gz
silicon-apps upload mcport --target linux-aarch64 mcport-0.3.0-linux-aarch64.tar.gz
silicon-apps upload mcport --target linux-armv7hf mcport-0.3.0-linux-armv7hf.tar.gz
silicon-apps packages mcport
silicon-apps release mcport --version 0.3.0 --package <id> --package <id> --package <id> --package <id>
silicon-apps promote mcport <development-release-id> --version 0.3.0
```

Every upload is validated again on its target's worker. A development release installs
with `silicon-apps install 'mcport>dev'`; `silicon-apps install mcport` takes the latest
production release once it is promoted. Publish the crates in dependency order
(`mcport-core`, `mcport-mcp`, `mcport-api`, `mcport-daemon`, `mcport-client`,
`mcport-cli`) with `cargo publish -p <crate>`.

## Backend candidate

`.github/workflows/backend.yml` builds the service on ARM64 Amazon Linux 2023 and packs a
service-only bundle with `scripts/package_backend.py`; [the deploy guide](../deploy/README.md)
installs it. The [systemd unit](../deploy/mcport.service) runs `mcport-server` from
`/opt/mcport/current` as the `mcport` user, keeps state in `/var/lib/mcport` and reads
`/etc/mcport/runtime.env` (Silicon Accounts URL, the `mcport` app secret, the webhook
secret, Postmark and telemetry keys; fields in `deploy/environment.example`). HTTPS
proxying to loopback port 4380 sits in front. The website deploys separately to Vercel
([deploy/vercel.md](../deploy/vercel.md)).
