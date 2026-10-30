#!/usr/bin/env python3
"""Real-process PostgreSQL connection-loss recovery, using only disposable fixtures.

Set MCPORT_TEST_DATABASE_URL to a fresh literal-loopback *_test database and put
psql on PATH (or set MCPORT_PSQL). Build mcport-server and mcport first. The only
terminated database session is the McPort writer identified by its advisory lock
inside that exact test database. No application data is injected or modified by SQL.
"""
import argparse
import concurrent.futures
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time
from unittest.mock import patch
import run as e2e


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    args = parser.parse_args()
    if not os.environ.get("MCPORT_TEST_DATABASE_URL"):
        parser.error("MCPORT_TEST_DATABASE_URL is required")
    args.run_dir.mkdir(parents=True, exist_ok=False)
    args.run_dir.chmod(0o700)
    journey = e2e.Journey(args.run_dir.resolve(), e2e.free_block())
    spec = importlib.util.spec_from_file_location("dev_accounts", e2e.ROOT / "scripts/dev_accounts.py")
    dev = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(dev)
    owned = []
    logs = []
    try:
        journey.start()
        with patch.dict(os.environ, journey.dev_env):
            cfg = dev.load_config()
            dev.stop(cfg, "service")
            env = dev.service_env(cfg, cfg["secret_file"].read_text().strip())

        def launch(name):
            log = (args.run_dir / (name + ".log")).open("w")
            logs.append(log)
            process = subprocess.Popen([journey.server_binary], cwd=e2e.ROOT, env=env,
                                       stdout=log, stderr=subprocess.STDOUT)
            owned.append(process)
            (cfg["pids"] / "service").write_text(str(process.pid))
            journey.ready(journey.backend + "/health", process)
            return process

        first = launch("first")
        journey.account("owner", "carbon", "recovery-owner")
        journey.login("owner")
        connection = journey.cli("owner", "connection", "new", "recovery", "--transport", "http",
                                 "--url", journey.provider + "/mcp/public", "--auth", "none")
        completed = journey.cli("owner", "tool", "call", "recovery", "write", "--input", '{"value":"keep this result"}')
        headers = journey.api_session()
        endpoint = journey.backend + "/api/v1/connections/recovery/mcp"
        rpc = {"method": "tools/call", "params": {"name": "slow", "arguments": {"seconds": 10}},
               "idempotency_key": "connection-loss-must-not-replay"}
        with concurrent.futures.ThreadPoolExecutor() as workers:
            pending = workers.submit(e2e.request, endpoint, rpc, None, headers, None)
            call_id = journey.wait("running call is persisted", lambda: next((c["id"] for c in e2e.request(
                journey.backend + "/api/v1/calls", headers=headers)["data"] if c["status"] == "running" and c.get("tool_name") == "slow"), None))
            journey.wait("provider actually started the call", lambda: any(c["tool"] == "slow" for c in e2e.request(journey.provider + "/fixture/stats")["calls"]))
            writer = journey.read_indexes("SELECT DISTINCT a.pid FROM pg_stat_activity a JOIN pg_locks l ON l.pid=a.pid WHERE a.datname=current_database() AND l.locktype='advisory' AND l.classid=1929374771 AND l.objid=1296257091 AND l.granted")
            assert len(writer) == 1, "expected exactly one writer in the disposable database"
            pid = int(writer[0][0])
            started = time.monotonic()
            assert journey.read_indexes(f"SELECT pg_terminate_backend({pid})") == [("t",)]
            status = first.wait(timeout=15)
            elapsed = time.monotonic() - started
            journey.check("Losing the database writer session stops the real backend with a nonzero exit", status != 0 and elapsed < 15)
            try:
                pending.result(timeout=5)
            except (OSError, e2e.URLError):
                pass  # An interrupted request is not a successful action receipt.
        second = launch("restarted")
        retained = journey.cli("owner", "connection", "show", "recovery")
        journey.check("Restart retains the connection identity", retained["id"] == connection["id"])
        old_call = e2e.request(journey.backend + "/api/v1/calls/" + completed["call_id"], headers=headers)["data"]
        journey.check("Restart retains completed result bytes", old_call["result"] == completed["result"])
        uncertain = e2e.request(journey.backend + "/api/v1/calls/" + call_id, headers=headers)["data"]
        journey.check("Pending dispatched call recovers as unknown without a second dispatch", uncertain["status"] == "unknown" and uncertain["error"]["code"] == "service_restarted" and uncertain["error"]["outcome_unknown"])
        replay = e2e.request(endpoint, rpc, headers=headers, expected=502)
        retained_calls=e2e.request(journey.backend + "/api/v1/calls",headers=headers)["data"]
        journey.check("Retrying the original idempotency key returns the unknown outcome without a new call", replay["error"]["code"] == "service_restarted" and replay["error"]["outcome_unknown"] and len([c for c in retained_calls if c.get("tool_name")=="slow"])==1)
        calls = e2e.request(journey.provider + "/fixture/stats")["calls"]
        journey.check("Provider saw exactly one dispatched slow call", sum(c["tool"] == "slow" for c in calls) == 1)
        result = {"passed": len(journey.checks), "checks": journey.checks, "storage": "postgresql",
                  "database_failure_exit_status": status, "failure_to_exit_seconds": round(elapsed,3),
                  "original_call_id": call_id, "recovered_status": uncertain["status"]}
        (args.run_dir / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result))
    finally:
        for process in owned:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        journey.close()
        for log in logs:
            log.close()


if __name__ == "__main__":
    main()
