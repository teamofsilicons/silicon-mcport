#!/usr/bin/env bash
# Stop what scripts/dev-accounts.sh started (the service and the MCP fixture providers). Data, logs and the
# webhook secret stay in .local/dev-accounts (or MCPORT_DEV_DIR) for the next start.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" down "$@"
