#!/usr/bin/env bash
# Start MCPort on this machine against a local Silicon Accounts stack (idempotent):
#
#   MCPORT_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build]
#
# Starts the MCP fixture providers (127.0.0.1:4242) and mcport-server (127.0.0.1:4241), points MCPort's webhook
# at Silicon Accounts to the service and proves a test delivery. Stop with scripts/dev-accounts-stop.sh.
# Configuration and details: python3 scripts/dev_accounts.py --help, and docs/development.md.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" up "$@"
