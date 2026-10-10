#!/usr/bin/env python3
"""Run local development gates without changing installed system dependencies."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def run(*command, cwd=ROOT):
    print("+ " + " ".join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), cwd=cwd, env={**os.environ, "MCPORT_TEST_PYTHON": sys.executable}, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-e2e", action="store_true", help="Skip the real-binary end-to-end journey and its fixture tests")
    parser.add_argument("--skip-web", action="store_true", help="Skip web tests/build if working only on Rust")
    parser.add_argument("--e2e-run-dir", type=Path, help="Retain isolated integration logs in this fresh directory")
    args = parser.parse_args()
    run("cargo", "fmt", "--all", "--check")
    run("cargo", "test", "--workspace", "--locked")
    run("cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    run(sys.executable, "-m", "unittest", "discover", "-s", "scripts/tests", "-p", "test_*.py")
    # The three commands Silicon Apps runs on every upload, against the CLI the tests just built.
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    cli = target_dir / "debug" / ("mcport.exe" if os.name == "nt" else "mcport")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    run(sys.executable, "-I", ROOT / "scripts" / "package_apps.py", "--check-only", version, "host", cli)
    if not args.skip_web:
        npm = shutil.which("npm") or shutil.which("npm.cmd")
        if not npm:
            parser.error("Node/npm is required for web checks; install it or pass --skip-web")
        run(npm, "ci", cwd=ROOT / "web")
        run(npm, "test", cwd=ROOT / "web")
        run(npm, "run", "build", cwd=ROOT / "web")
    if not args.skip_e2e:
        run(sys.executable, "-m", "unittest", "discover", "-s", "tests/e2e", "-p", "test_*.py")
        options = ["--run-dir", args.e2e_run_dir] if args.e2e_run_dir else []
        run(sys.executable, "tests/e2e/run.py", *options)


if __name__ == "__main__":
    main()
