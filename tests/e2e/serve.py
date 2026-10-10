#!/usr/bin/env python3
"""Start the real service against the fake Silicon Accounts and the MCP fixtures, for exploring by hand.

    python3 tests/e2e/serve.py [--base 4250] [--run-dir DIR] [--no-build]

Ports: base+1 service, base+2 MCP fixtures, base+3 fake Silicon Accounts. Prints the URLs, a Carbon c:owner and a
Silicon si:researcher in its care, and how to sign each in. Press Ctrl-C to stop everything it started.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).parent))
from run import Journey  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", type=int, default=4250)
    parser.add_argument("--run-dir", type=Path, default=ROOT / ".local/e2e/manual")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    run_dir = args.run_dir.resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir.chmod(0o700)
    if not args.no_build:
        subprocess.run(["cargo", "build", "--locked", "-p", "mcport-cli", "-p", "mcport-server"], cwd=ROOT, check=True)
    journey = Journey(run_dir, args.base)
    try:
        journey.start()
        owner = journey.account("owner", "carbon", "owner")
        silicon = journey.account("silicon", "silicon", "researcher", custodian="owner")
        env = f"MCPORT_URL={journey.backend} ACCOUNTS_URL={journey.accounts} SILICON_HOME=<a home per identity>"
        print(json.dumps({
            "backend_url": journey.backend, "accounts_url": journey.accounts, "providers": journey.provider,
            "cli": journey.cli_binary, "carbon": owner, "silicon": silicon,
            "sign_in": {
                "carbon": f"{env} mcport login   # then: curl -X POST {journey.accounts}/fixture/device/<CODE>/approve -H 'Content-Type: application/json' -d '{{\"uuid\":\"{owner['uuid']}\"}}'",
                "silicon": f"curl -s -X POST {journey.accounts}/fixture/slt -H 'Content-Type: application/json' -d '{{\"uuid\":\"{silicon['uuid']}\"}}'   # then: {env} mcport login --slt-stdin",
            },
            "mcp_fixtures": {"public": journey.provider + "/mcp/public", "bearer": journey.provider + "/mcp/bearer",
                             "oauth": journey.provider + "/mcp/oauth", "local": journey.provider + "/mcp/local"},
            "run_dir": str(run_dir),
        }, indent=2), flush=True)
        print("Ready. Every credential here is a fixture. Press Ctrl-C to stop.", flush=True)
        signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
        while all(child.poll() is None for child in journey.children):
            time.sleep(0.5)
    except KeyboardInterrupt:
        pass
    finally:
        journey.close()
        if os.name == "posix":
            print("Stopped.", flush=True)


if __name__ == "__main__":
    main()
