#!/usr/bin/env python3
"""AL2023 native build evidence and a disposable unauthenticated health probe.

Run only after the preceding native cargo tests succeed. This is not Silicon
Accounts or provider verification. It never reads deployment credentials or persistent data.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    release = Path("/etc/os-release").read_text()
    if platform.system() != "Linux" or platform.machine() != "aarch64" or 'ID="amzn"' not in release or 'VERSION_ID="2023"' not in release:
        raise RuntimeError("Native evidence must be produced on Amazon Linux 2023 ARM64")
    binary = args.binary.resolve()
    linked = subprocess.check_output(["ldd", str(binary)], text=True)
    versions = subprocess.check_output(["readelf", "--version-info", str(binary)], text=True)
    glibc_versions = {tuple(map(int, value.split("."))) for value in re.findall(r"GLIBC_(\d+\.\d+)", versions)}
    if "not found" in linked or not glibc_versions or max(glibc_versions) > (2, 34):
        raise RuntimeError("Binary exceeds AL2023 glibc2.34 baseline or lacks a required library")
    with socket.socket() as available:
        available.bind(("127.0.0.1", 0))
        port = available.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="mcport-backend-smoke-") as directory:
        env = {key: value for key, value in os.environ.items() if not key.startswith(("MCPORT_", "POSTMARK_", "ACCOUNTS_", "SPACE_STATION"))}
        # Health needs no Silicon Accounts call; an unreachable loopback URL proves that.
        env.update(MCPORT_BIND="127.0.0.1:" + str(port), MCPORT_DATA_DIR=directory, MCPORT_APP_SECRET="candidate-smoke-fixture-not-an-app-secret", ACCOUNTS_URL="http://127.0.0.1:1")
        with tempfile.TemporaryFile() as logs:
            process = subprocess.Popen([str(binary)], env=env, stdout=logs, stderr=logs)
            try:
                version = None
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                for _ in range(100):
                    if process.poll() is not None:
                        raise RuntimeError("Candidate exited before health became available")
                    try:
                        with opener.open("http://127.0.0.1:%s/health" % port, timeout=1) as response:
                            value = json.loads(response.read(4096))
                            if value.get("status") == "ok":
                                if value.get("source_revision") != args.revision:
                                    raise RuntimeError("Candidate health source revision does not match the build")
                                version = value["version"]
                                break
                    except OSError:
                        pass
                    time.sleep(0.1)
                if not version:
                    raise RuntimeError("Candidate health did not become available")
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                        raise RuntimeError("Candidate did not shut down gracefully")
            if process.returncode != 0:
                raise RuntimeError("Candidate exited with an error during shutdown")
    evidence = {"source_revision": args.revision, "target": "aarch64-unknown-linux-gnu", "build_distribution": "Amazon Linux 2023", "glibc_baseline": "2.34", "max_required_glibc": ".".join(map(str, max(glibc_versions))), "build_image": args.image, "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "native_tests_passed": True, "native_health_smoke_passed": True, "health_version": version, "health_source_revision": args.revision}
    with args.output.open("x") as destination:
        json.dump(evidence, destination, indent=2)
        destination.write("\n")


if __name__ == "__main__":
    main()
