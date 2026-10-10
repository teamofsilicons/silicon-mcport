#!/usr/bin/env bash
# Package one target of the mcport CLI for Silicon Apps:
#
#   scripts/package-apps.sh [options] VERSION TARGET BINARY
#   scripts/package-apps.sh 0.3.0 linux-x86_64 target/x86_64-unknown-linux-musl/release/mcport
#
# Writes dist/apps/mcport-VERSION-TARGET.tar.gz and its .sha256 after the binary
# answered --help, accounts --json and login status --json in an empty home
# (whenever this machine can run it) and `silicon-apps validate` accepted the
# package. --check-only runs just the binary checks. Run with --help for every
# option; scripts/README.md explains the release flow.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python="${PYTHON:-}"
if [ -z "$python" ]; then
  for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 \
      && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 11))' >/dev/null 2>&1; then
      python="$candidate"
      break
    fi
  done
fi
if [ -z "$python" ]; then
  echo "package-apps: Python 3.11 or newer is required (it reads Cargo.toml with tomllib); install it or set PYTHON." >&2
  exit 2
fi
exec "$python" -I "$here/package_apps.py" "$@"
