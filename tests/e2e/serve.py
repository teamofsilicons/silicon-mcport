#!/usr/bin/env python3
"""Start the real backend against isolated, loopback-only IAM and MCP fixtures."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
from urllib.error import URLError
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[2]


def ready(url, process):
    for _ in range(150):
        if process.poll() is not None:
            raise RuntimeError("A fixture or backend process exited before readiness; inspect the run directory logs")
        try:
            with urlopen(url, timeout=1) as response:
                if response.status == 200:
                    return
        except (URLError, OSError):
            pass
        time.sleep(0.1)
    raise RuntimeError("Service did not become ready at " + url)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-port", type=int, default=4390)
    parser.add_argument("--backend-port", type=int, default=4380)
    parser.add_argument("--run-dir", type=Path, default=ROOT / ".local/e2e/manual")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    run_dir = args.run_dir.resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    run_dir.chmod(0o700)
    if not args.no_build:
        subprocess.run(["cargo", "build", "-p", "mcport-cli", "-p", "mcport-server"], cwd=ROOT, check=True)
    children = []
    logs = []
    try:
        fixture_log = (run_dir / "fixtures.log").open("w")
        logs.append(fixture_log)
        fixture = subprocess.Popen([sys.executable, str(ROOT / "tests/e2e/fixtures.py"), "--port", str(args.fixture_port), "--state", str(run_dir / "fixtures.json")], stdout=fixture_log, stderr=subprocess.STDOUT)
        children.append(fixture)
        origin = f"http://127.0.0.1:{args.fixture_port}"
        ready(origin + "/health", fixture)
        env = dict(os.environ, MCPORT_BIND=f"127.0.0.1:{args.backend_port}", MCPORT_PUBLIC_URL=f"http://127.0.0.1:{args.backend_port}", MCPORT_IAM_URL=origin, MCPORT_IAM_WEB_URL=origin, MCPORT_APP_SECRET="fixture-app-secret", MCPORT_ALLOWED_UPSTREAM_ORIGINS=origin, MCPORT_DATA_DIR=str(run_dir / "server"), MCPORT_LIFECYCLE_SECRET="fixture-lifecycle-secret-0123456789abcdef", MCPORT_WEBHOOK_SECRET="fixture-webhook-secret-0123456789abcdef", MCPORT_TEST_APP_SECRETS=json.dumps({"11111111-1111-4111-8111-111111111111": "fixture-test-app-secret"}))
        # Do not accidentally send fixture telemetry or bug mail with inherited production credentials.
        for key in ("POSTMARK_SERVER_TOKEN", "SPACE_STATION_API_KEY", "SPACESTATION_API_KEY", "MCPORT_TELEMETRY_KEY", "MCPORT_TEST_TELEMETRY_KEYS"):
            env.pop(key, None)
        backend_log = (run_dir / "backend.log").open("w")
        logs.append(backend_log)
        backend = subprocess.Popen([str(ROOT / "target/debug/mcport-server")], cwd=ROOT, env=env, stdout=backend_log, stderr=subprocess.STDOUT)
        children.append(backend)
        backend_url = f"http://127.0.0.1:{args.backend_port}"
        ready(backend_url + "/api/v1/iam", backend)
        metadata = json.loads((run_dir / "fixtures.json").read_text())
        metadata.update(backend_url=backend_url, run_dir=str(run_dir), cli=str(ROOT / "target/debug/mcport"))
        (run_dir / "ready.json").write_text(json.dumps(metadata, indent=2))
        (run_dir / "ready.json").chmod(0o600)
        print(json.dumps(metadata, indent=2), flush=True)
        print("Ready. Use separate SILICON_HOME directories for owner/silicon/stranger/crossorg. Press Ctrl-C to stop these two processes.", flush=True)
        while all(child.poll() is None for child in children):
            time.sleep(0.5)
    except KeyboardInterrupt:
        pass
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.send_signal(signal.SIGTERM)
        for child in reversed(children):
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for log in logs:
            log.close()


if __name__ == "__main__":
    main()
