# Development and native release candidates

Create a local Python environment for packaging helpers, then run the complete local gates:

```sh
python3 -m venv .local/devtools
.local/devtools/bin/python -m pip install -r scripts/requirements.txt
.local/devtools/bin/python scripts/check.py
```

`check.py` runs Rust formatting, workspace tests and strict Clippy, packaging rejection tests, IAM/MCP fixture tests, web tests/build, and the real-binary end-to-end journey. `--skip-web` and `--skip-e2e` are explicit narrower developer checks. It installs no system dependencies. Use Node 24+, Python 3.11+ and the current stable Rust toolchain.

The native workflow uses real x86-64 and ARM64 runners for Linux, macOS and Windows. Runner labels are selected from [GitHub's hosted-runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners); actions are pinned by commit SHA. Each job tests the CLI, SDK, daemon and MCP transports on its native architecture, builds the release CLI, executes `--version`, checks the executable format/CPU and uploads a target archive. The local daemon is embedded in `mcport`; no second daemon executable is installed. Linux GNU artifacts use Ubuntu 24.04 as their tested runtime baseline; compatibility with older glibc distributions requires separate verification.

To stage a real local ARM64 macOS build:

```sh
cargo build --locked --release -p mcport-cli --target aarch64-apple-darwin
.local/devtools/bin/python scripts/package.py stage \
  --target macos-aarch64 \
  --binary target/aarch64-apple-darwin/release/mcport \
  --output .local/native/mcport-macos-aarch64.zip
```

`stage` must run on the matching native host. Its ZIP preserves executable bits through artifact upload/download. The assembler requires all six archives from the same candidate run:

```sh
.local/devtools/bin/python scripts/package.py assemble \
  --input .local/native \
  --output .local/candidate/mcport-honeycomb.tar.gz
```

Both commands verify the exact manifest version and paths. Assembly rechecks ELF64, Mach-O64 or PE32+ architecture, permissions, native smoke metadata and binary SHA-256; missing, extra, duplicate, truncated, wrong-architecture and altered payloads fail. Native handoffs use stored ZIP entries. The complete candidate uses sorted USTAR entries and gzip with a fixed timestamp and empty filename for deterministic bytes. Existing archives are never overwritten. The complete `.tar.gz` contains only root `honeycomb.yaml` and declared binaries below `targets/`, as Honeycomb requires. Build records stay in a `.provenance.json` sidecar, alongside `.sha256` and `SHA256SUMS`. Assembly enforces the official 2 GiB expanded / 512 MiB compressed limits. CI installs the official `silicon-honeycomb-cli` 0.6.1 and requires `honeycomb validate` on the final archive before uploading the candidate artifact. Locally, use `honeycomb validate <archive.tar.gz>`; `honeycomb pack <populated-directory> --output <archive.tar.gz>` is the official alternative packer. Tests use synthetic headers only to exercise validation; the staging command never fabricates executables.

The workflow produces downloadable candidate artifacts only. It has read-only repository permissions and no publish/deploy step. Six green native jobs and the assembled artifact are required before claiming all target builds were verified. A local test pass does not establish those remote results.

The [native systemd unit](../deploy/mcport.service) runs the backend and built website from `/opt/mcport/current` as an existing `mcport` service user. Install `mcport-server` and `web/dist` there, supply protected `/etc/mcport/runtime.env`, and retain `/var/lib/mcport` across releases. Required live IAM/app settings, public HTTPS origins, Honeycomb lifecycle/webhook secrets, Postmark configuration and environment-specific telemetry keys are operator configuration. Put HTTPS reverse proxying in front of loopback port 4380. The unit's SIGINT stop signal lets the server finish its existing graceful shutdown. Installing/enabling the service is a separate deployment action.
