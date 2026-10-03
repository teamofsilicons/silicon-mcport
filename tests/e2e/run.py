#!/usr/bin/env python3
"""Regression journey through real MCPort binaries, official IAM and MCP fixtures.

Uses fresh temporary homes/data and ephemeral ports. Never touches port 4380/4390,
production IAM, Postmark, or production Space Station. Prints only fixture data.
"""
import argparse
import concurrent.futures
import html
import http.cookiejar
import re
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen, build_opener, HTTPCookieProcessor

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).parent))
from fixtures import TEST_ID, TEST_KEY


def free_port():
    with socket.socket() as handle:
        handle.bind(("127.0.0.1", 0))
        return handle.getsockname()[1]


def request(url, body=None, method=None, headers=None, expected=200):
    data = json.dumps(body).encode() if body is not None else None
    options = {"Content-Type": "application/json", **(headers or {})}
    try:
        with urlopen(Request(url, data=data, method=method, headers=options), timeout=150) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        status, raw = error.code, error.read()
    value = json.loads(raw) if raw else None
    if expected is not None and status != expected:
        raise AssertionError(f"HTTP {status}, expected {expected}, {url}: {value}")
    return value


class Journey:
    def __init__(self, directory):
        self.directory = directory
        self.children = []
        self.logfiles = []
        self.checks = []
        self.homes = {}
        self.backend = "http://127.0.0.1:" + str(free_port())
        self.provider = "http://127.0.0.1:" + str(free_port())
        self.cli_binary = str(ROOT / "target/debug/mcport")

    def spawn(self, name, command, env=None):
        log = (self.directory / (name + ".log")).open("w")
        self.logfiles.append(log)
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        self.children.append(process)
        return process

    def ready(self, url, process):
        for _ in range(200):
            if process.poll() is not None:
                raise AssertionError("Service exited before readiness; inspect " + str(self.directory))
            try:
                request(url)
                return
            except (URLError, OSError):
                time.sleep(0.1)
        raise AssertionError("Readiness timed out: " + url)

    def start(self):
        fixture = self.spawn("fixture", [sys.executable, str(ROOT / "tests/e2e/fixtures.py"), "--port", self.provider.rsplit(":", 1)[1]])
        self.ready(self.provider + "/health", fixture)
        env = dict(os.environ, MCPORT_BIND=self.backend.removeprefix("http://"), MCPORT_PUBLIC_URL=self.backend, MCPORT_IAM_URL=self.provider, MCPORT_IAM_WEB_URL=self.provider, MCPORT_APP_SECRET="fixture-app-secret", MCPORT_DATA_DIR=str(self.directory / "server"), MCPORT_ALLOWED_UPSTREAM_ORIGINS=self.provider, MCPORT_LIFECYCLE_SECRET="fixture-lifecycle-secret-0123456789abcdef", MCPORT_WEBHOOK_SECRET="fixture-webhook-secret-0123456789abcdef", MCPORT_TEST_APP_SECRETS=json.dumps({TEST_ID: "fixture-test-app-secret"}))
        for key in ("POSTMARK_SERVER_TOKEN", "MCPORT_TELEMETRY_KEY", "MCPORT_TEST_TELEMETRY_KEYS"):
            env.pop(key, None)
        backend = self.spawn("backend", [str(ROOT / "target/debug/mcport-server")], env)
        self.ready(self.backend + "/api/v1/iam", backend)

    def home(self, role):
        if role not in self.homes:
            self.homes[role] = self.directory / "homes" / role
            self.homes[role].mkdir(parents=True)
        return self.homes[role]

    def cli(self, role, *args, expected=True, stdin=None, test=None):
        command = [self.cli_binary, "--json"]
        if test:
            command += ["--test", test]
        command += list(args)
        env = dict(os.environ, SILICON_HOME=str(self.home(role)), MCPORT_URL=self.backend)
        result = subprocess.run(command, input=stdin, text=True, capture_output=True, env=env, timeout=160)
        try:
            value = json.loads(result.stdout)
        except ValueError:
            raise AssertionError(f"CLI returned non-JSON: {args[:3]} {result.stdout} {result.stderr}") from None
        if (result.returncode == 0) != expected:
            safe_args = args[:1] if args and args[0] == "login" else args
            raise AssertionError(f"[{role}] {safe_args}: status {result.returncode}; {value}; stderr={result.stderr}")
        return value

    def check(self, label, condition=True):
        if not condition:
            raise AssertionError(label)
        self.checks.append(label)
        print("PASS " + label, flush=True)

    def login(self, home, role=None, test=None):
        slt = request(self.provider + "/fixture/slt", {"role": role or home, "environment": test or "production"})["slt"]
        result = self.cli(home, "login", slt, test=test)
        assert result["authenticated"] is True
        return result

    def api_session(self, role="owner", test=None):
        slt = request(self.provider + "/fixture/slt", {"role": role, "environment": test or "production"})["slt"]
        headers = {"X-MCPort-Test": test} if test else {}
        session = request(self.backend + "/api/v1/auth/login", {"slt": slt}, headers=headers)["data"]
        return {"Authorization": "Bearer " + session["access_token"], **headers}

    def lifecycle(self, action, revision, generation=1, operation=None):
        operation = operation or str(uuid.uuid4())
        body = {"app_id": "mcport", "environment_id": TEST_ID, "operation_id": operation, "org_id": "tos", "environment_revision": revision, "generation": generation, "key_version": 1, "action": action, "testing_key": TEST_KEY, "snapshot": {}}
        url = self.backend + f"/internal/honeycomb/organizations/tos/testing-environments/{TEST_ID}/operations/{operation}"
        result = request(url, body, method="PUT", headers={"Authorization": "Bearer fixture-lifecycle-secret-0123456789abcdef"})
        assert result["state"] == "completed"
        return url, body

    def authority_index(self):
        # Read only non-secret record indexes. All authority is created/removed
        # through the real API; no ciphertext, master key or database writes.
        database = (self.directory / "server" / "mcport.sqlite").as_uri() + "?mode=ro"
        with sqlite3.connect(database, uri=True) as connection:
            return set(connection.execute(
                "SELECT kind,id FROM records WHERE environment='production' "
                "AND kind IN ('connection','credential','account_epoch','grant','policy','oauth_attempt')"
            ).fetchall())

    def wait_connection(self, name, ready=True):
        for _ in range(160):
            response = self.cli("owner", "connection", "show", name)
            if (response["status"] == "ready") == ready:
                return response
            time.sleep(0.3)
        raise AssertionError("Connection readiness did not change: " + name)

    def wait_status(self, name, status):
        for _ in range(160):
            response = self.cli("owner", "connection", "show", name)
            if response["status"] == status:
                return response
            time.sleep(0.3)
        raise AssertionError("Connection did not reach " + status + ": " + name)

    def run(self):
        self.check("IAM discovery before login", self.cli("owner", "iam")["app_id"] == "mcport")
        for role in ("owner", "silicon", "stranger", "crossorg"):
            self.login(role)
            self.check("IAM " + role + " login", self.cli(role, "login", "status")["authenticated"])
        self.cli("owner", "connection", "new", "public", "--transport", "http", "--url", self.provider + "/mcp/public", "--auth", "none", "--visibility", "private")
        self.check("Ordinary org member creates a connection")
        self.check("Private connection hidden from another org member", not self.cli("stranger", "connection", "ls"))
        self.cli("stranger", "tool", "ls", "public", expected=False)
        self.check("Private invocation denied")
        self.cli("owner", "connection", "set", "public", "--visibility", "org")
        self.check("Org connection visible in same org", len(self.cli("stranger", "connection", "ls")) == 1)
        self.cli("crossorg", "tool", "ls", "public", expected=False)
        self.check("Org visibility does not cross organizations")
        self.cli("owner", "connection", "set", "public", "--visibility", "invited")
        self.cli("owner", "access", "new", "public", "--principal", "si:researcher")
        page = self.cli("silicon", "tool", "ls", "public")["result"]
        self.check("Invited Silicon discovers default-enabled tools", all(tool["enabled"] for tool in page["tools"]))
        second = self.cli("silicon", "tool", "ls", "public", "--cursor", page["nextCursor"])["result"]
        self.check("Tool pagination preserved", any(tool["name"] == "nested" for tool in second["tools"]))
        self.check("Tool show traverses pages", self.cli("silicon", "tool", "show", "public", "nested")["name"] == "nested")
        inline = self.cli("silicon", "tool", "call", "public", "echo", "--input", '{"message":"inline"}')
        self.check("Tool JSON invocation", inline["result"]["structuredContent"]["arguments"]["message"] == "inline")
        payload = self.directory / "input.json"
        payload.write_text('{"message":"file"}')
        self.check("Tool file input", self.cli("silicon", "tool", "call", "public", "echo", "--input", "@" + str(payload))["result"]["structuredContent"]["arguments"]["message"] == "file")
        self.check("Tool stdin input", self.cli("silicon", "tool", "call", "public", "echo", "--input", "-", stdin='{"message":"stdin"}')["result"]["structuredContent"]["arguments"]["message"] == "stdin")
        self.cli("silicon", "tool", "call", "public", "echo", "--input", '{"message":42}', expected=False)
        self.check("Input schema rejection before tool execution")
        nested_call = self.cli("silicon", "tool", "call", "public", "nested", "--input", '{"options":{"tags":["a","b"]}}')
        nested = nested_call["result"]
        self.check("Text image resource-link and structured output preserved", len(nested["content"]) == 3 and nested["structuredContent"]["arguments"]["options"]["tags"] == ["a", "b"])
        assets = self.cli("silicon", "asset", "ls", nested_call["call_id"])
        image_asset = next(asset for asset in assets if asset["mime_type"] == "image/png")
        saved = self.directory / "fixture-image.png"
        self.cli("silicon", "asset", "get", nested_call["call_id"], str(image_asset["index"]), "--output", str(saved))
        original = saved.read_bytes()
        self.check("Authorized embedded asset saves privately", bool(original) and (os.name != "posix" or saved.stat().st_mode & 0o777 == 0o600))
        self.cli("silicon", "asset", "get", nested_call["call_id"], str(image_asset["index"]), "--output", str(saved), expected=False)
        self.check("Asset output never overwrites an existing file", saved.read_bytes() == original)
        self.cli("owner", "asset", "ls", nested_call["call_id"], expected=False)
        self.check("Connection ownership does not reveal another caller's assets")
        persisted = self.cli("silicon", "activity", "show", nested_call["call_id"])
        self.check("Authorized full-result GET preserves stored content", persisted["result"] == nested)
        self.cli("owner", "tool", "set", "public", "nested", "--enabled", "false")
        self.cli("silicon", "asset", "get", nested_call["call_id"], str(image_asset["index"]), "--output", str(self.directory / "denied.png"), expected=False)
        self.check("Current tool revocation blocks saved result downloads", not (self.directory / "denied.png").exists())
        denied_result = self.cli("silicon", "activity", "show", nested_call["call_id"], expected=False)
        self.check("Current tool revocation blocks full-result GET", "error" in denied_result and "result" not in denied_result)
        cancelled_completed = self.cli("silicon", "activity", "cancel", nested_call["call_id"])
        self.check("Cancel response cannot bypass revoked result access", cancelled_completed.get("result") is None)
        self.cli("owner", "tool", "set", "public", "nested", "--enabled", "true")
        self.check("Restored tool access permits full-result GET", self.cli("silicon", "activity", "show", nested_call["call_id"])["result"] == nested)
        self.check("MCP tool errors produce nonzero exit and retain result", self.cli("silicon", "tool", "call", "public", "fail", expected=False)["result"]["isError"])
        self.check("Resources supported", self.cli("silicon", "resource", "read", "public", "fixture://readme")["result"]["contents"][0]["text"].startswith("Fixture resource"))
        self.check("Resource templates supported", bool(self.cli("silicon", "resource", "templates", "public")["result"]["resourceTemplates"]))
        self.check("Prompts supported", self.cli("silicon", "prompt", "get", "public", "summarize", "--input", '{"text":"hello"}')["result"]["messages"][0]["content"]["text"] == "Summarize: hello")
        completion = self.cli("silicon", "completion", "get", "public", "--input", '{"ref":{"type":"ref/prompt","name":"summarize"},"argument":{"name":"text","value":"hel"}}')
        self.check("Completion params and returned suggestions preserved", completion["result"]["completion"]["values"] == ["hel-fixture"])
        self.cli("owner", "tool", "set", "public", "echo", "--principal", "si:researcher", "--enabled", "false")
        self.cli("silicon", "tool", "call", "public", "echo", "--input", '{"message":"denied"}', expected=False)
        self.cli("owner", "tool", "call", "public", "echo", "--input", '{"message":"owner allowed"}')
        self.check("Per-user disabled tool leaves owner access intact")
        self.cli("owner", "tool", "set", "public", "echo", "--enabled", "false")
        self.cli("owner", "tool", "set", "public", "echo", "--principal", "si:researcher", "--enabled", "true")
        self.cli("silicon", "tool", "call", "public", "echo", "--input", '{"message":"denied"}', expected=False)
        self.check("Per-user enable cannot bypass connection-wide disable")
        self.cli("owner", "tool", "set", "public", "echo", "--enabled", "true")
        for name, mode in (("shared", "shared"), ("personal", "per-user")):
            self.cli("owner", "connection", "new", name, "--transport", "http", "--url", self.provider + "/mcp/bearer", "--auth", mode, "--visibility", "org")
            self.cli("owner", "account", "connect", name, "--input", '{"kind":"bearer","secret":"fixture-owner-token","label":"Owner fixture provider"}')
        shared = self.cli("silicon", "tool", "call", "shared", "whoami")
        self.check("Remote Silicon deliberately uses creator's shared provider grant", shared["result"]["structuredContent"]["account"] == "provider-owner")
        self.cli("silicon", "tool", "call", "personal", "whoami", expected=False)
        self.check("Missing personal grant never falls back to owner's grant")
        self.cli("silicon", "account", "connect", "personal", "--input", '{"kind":"bearer","secret":"fixture-silicon-token","label":"Silicon fixture provider"}')
        self.check("Personal account isolation", self.cli("silicon", "tool", "call", "personal", "whoami")["result"]["structuredContent"]["account"] == "provider-silicon")
        self.cli("silicon", "account", "disconnect", "personal")
        self.cli("silicon", "tool", "call", "personal", "whoami", expected=False)
        self.check("Disconnect immediately blocks further personal use")
        self.cli("owner", "connection", "new", "oauth", "--transport", "http", "--url", self.provider + "/mcp/oauth", "--auth", "shared", "--visibility", "org")
        authorization = self.cli("owner", "account", "connect", "oauth")
        browser = build_opener(HTTPCookieProcessor(http.cookiejar.CookieJar()))
        with browser.open(authorization["authorization_url"]) as response:
            consent = response.read().decode()
        provider_url = html.unescape(re.search(r"href=['\"]([^'\"]+)", consent).group(1))
        with browser.open(provider_url) as response:
            assert response.status == 200
        self.check("OAuth PKCE consent binds creator grant for remote Silicon", self.cli("silicon", "tool", "call", "oauth", "whoami")["result"]["structuredContent"]["account"] == "oauth-owner")
        authority_before = self.authority_index()
        self.cli("owner", "connection", "new", "delete-authority", "--transport", "http", "--url", self.provider + "/mcp/oauth", "--auth", "per-user", "--visibility", "invited")
        self.cli("owner", "access", "new", "delete-authority", "--principal", "si:researcher")
        self.cli("owner", "tool", "set", "delete-authority", "echo", "--enabled", "false")
        for role, token in (("owner", "fixture-owner-token"), ("silicon", "fixture-silicon-token")):
            self.cli(role, "account", "connect", "delete-authority", "--input", json.dumps({"kind":"bearer","secret":token}))
        self.cli("silicon", "account", "connect", "delete-authority")
        created_authority = self.authority_index() - authority_before
        assert {kind for kind, _ in created_authority} == {"connection", "credential", "account_epoch", "grant", "policy", "oauth_attempt"}
        assert sum(kind == "credential" for kind, _ in created_authority) == 2
        self.cli("owner", "connection", "rm", "delete-authority")
        self.check("Connection deletion removes both personal grants, policy, invite and pending OAuth authority", not (created_authority & self.authority_index()))
        self.cli("silicon", "account", "show", "delete-authority", expected=False)
        self.check("Connection deletion preserves another connection's shared grant", self.cli("silicon", "tool", "call", "shared", "whoami")["result"]["structuredContent"]["account"] == "provider-owner")
        self.cli("owner", "host", "new", "laptop")
        self.cli("owner", "connection", "new", "desktop", "--host", "laptop", "--transport", "http", "--url", self.provider + "/mcp/local", "--auth", "shared", "--visibility", "org")
        self.wait_connection("desktop")
        self.check("Remote home invokes registered local HTTP MCP", self.cli("silicon", "tool", "call", "desktop", "whoami")["result"]["structuredContent"]["account"] == "desktop-owner")
        self.cli("owner", "connection", "new", "stdio", "--host", "laptop", "--transport", "stdio", "--command", sys.executable, "--arg", str(ROOT / "tests/e2e/fixtures.py"), "--arg=--stdio", "--env", "FIXTURE_ACCOUNT=stdio-isolated", "--auth", "none", "--visibility", "org")
        self.wait_connection("stdio")
        self.check("Remote home invokes allowlisted local stdio process", self.cli("silicon", "tool", "call", "stdio", "whoami")["result"]["structuredContent"]["account"] == "stdio-isolated")
        self.cli("owner", "connection", "new", "not-mcp", "--host", "laptop", "--transport", "http", "--url", self.provider + "/not-an-mcp", "--auth", "none", "--visibility", "org")
        self.wait_status("not-mcp", "offline")
        self.check("An open HTTP port without MCP is offline while its host remains online", self.cli("owner", "host", "show", "laptop")["online"])
        missing = str((self.directory / "missing-mcp-program").resolve())
        self.cli("owner", "connection", "new", "missing-program", "--host", "laptop", "--transport", "stdio", "--command", missing, "--auth", "none", "--visibility", "org")
        self.wait_status("missing-program", "offline")
        self.check("A missing stdio executable is offline while working connections still run", self.cli("silicon", "tool", "call", "desktop", "whoami")["result"]["structuredContent"]["account"] == "desktop-owner")
        self.cli("owner", "account", "disconnect", "desktop")
        self.cli("silicon", "tool", "call", "desktop", "whoami", expected=False)
        self.check("Local shared disconnect cannot fall back to desktop account")
        self.cli("owner", "account", "connect", "desktop")
        self.wait_connection("desktop")
        self.cli("owner", "daemon", "stop")
        self.wait_connection("desktop", ready=False)
        self.cli("silicon", "tool", "call", "desktop", "whoami", expected=False)
        self.check("Offline local host never silently reroutes")
        self.cli("owner", "daemon", "start")
        self.wait_connection("desktop")
        self.cli("silicon", "tool", "call", "desktop", "whoami")
        self.check("Local host reconnect recovers service")
        headers = self.api_session()
        endpoint = self.backend + "/api/v1/connections/public/mcp"
        rpc = {"method": "tools/call", "params": {"name": "write", "arguments": {"value": "single logical write"}}, "idempotency_key": "fixture-idempotency-key"}
        first = request(endpoint, rpc, headers=headers)["data"]
        second = request(endpoint, rpc, headers=headers)["data"]
        calls = request(self.provider + "/fixture/stats")["calls"]
        self.check("Same idempotency key returns same result without replay", first == second and sum(call["tool"] == "write" for call in calls) == 1)
        rpc["params"]["arguments"]["value"] = "changed body"
        self.check("Idempotency key rejects changed request", request(endpoint, rpc, headers=headers, expected=409)["error"]["code"] == "idempotency_conflict")
        dropped = self.cli("owner", "tool", "call", "public", "drop", "--input", '{"value":"do not replay"}', expected=False)
        calls = request(self.provider + "/fixture/stats")["calls"]
        self.check("Lost upstream response reports unknown outcome with one dispatch", dropped["error"]["outcome_unknown"] and sum(call["tool"] == "drop" for call in calls) == 1)
        with concurrent.futures.ThreadPoolExecutor() as workers:
            pending = workers.submit(request, endpoint, {"method":"tools/call","params":{"name":"slow","arguments":{"seconds":4}},"idempotency_key":"fixture-cancel-key"}, None, headers, None)
            call_id = None
            for _ in range(80):
                calls = request(self.backend + "/api/v1/calls", headers=headers)["data"]
                active = next((call for call in calls if call["status"] == "running" and call.get("tool_name") == "slow"), None)
                if active:
                    call_id = active["id"]
                    break
                time.sleep(0.1)
            assert call_id, "Slow call did not become observable"
            request(self.backend + "/api/v1/calls/" + call_id + "/cancel", {}, headers=headers)
            pending.result(timeout=15)
            final = request(self.backend + "/api/v1/calls/" + call_id, headers=headers)["data"]
            self.check("Cancellation is observable and is not overwritten by late completion", final["status"] == "cancelled")
        self.cli("owner", "access", "rm", "public", "--principal", "si:researcher")
        self.cli("silicon", "tool", "ls", "public", expected=False)
        self.check("Invitation revocation defeats cached discovery")
        revoked_result = self.cli("silicon", "activity", "show", nested_call["call_id"], expected=False)
        self.check("Invitation revocation also blocks full-result GET", "error" in revoked_result and "result" not in revoked_result)

        request(self.provider + "/fixture/revoke", {"principal_id": "si:researcher"})
        self.cli("silicon", "connection", "ls", expected=False)
        self.check("IAM revocation checked live on existing gateway session")
        request(self.provider + "/fixture/revoke", {"principal_id": "si:researcher", "revoked": False})
        self.lifecycle("prepare", 1)
        self.login("owner", test=TEST_ID)
        self.check("Test session cannot see production connections", self.cli("owner", "connection", "ls", test=TEST_ID) == [])
        self.cli("owner", "connection", "new", "test-only", "--transport", "http", "--url", self.provider + "/mcp/public", "--visibility", "private", test=TEST_ID)
        self.cli("owner", "tool", "call", "test-only", "echo", "--input", '{"message":"isolated"}', test=TEST_ID)
        self.check("Provisioned test credentials run against isolated connections")
        test_report = self.cli("owner", "report", "Fixture bug report only", test=TEST_ID)
        self.check("Testing bug report never sends live email", test_report["status"] == "test_recorded")
        self.lifecycle("disable", 2)
        self.cli("owner", "connection", "ls", test=TEST_ID, expected=False)
        self.check("Disabled test environment rejects old sessions")
        self.lifecycle("restore", 3)
        self.cli("owner", "connection", "ls", test=TEST_ID, expected=False)
        self.login("owner", test=TEST_ID)
        self.check("Restore requires fresh login and retains isolated data", len(self.cli("owner", "connection", "ls", test=TEST_ID)) == 1)
        url, body = self.lifecycle("clean", 4, generation=2)
        replay = request(url, body, method="PUT", headers={"Authorization": "Bearer fixture-lifecycle-secret-0123456789abcdef"})
        self.check("Lifecycle replay returns completed receipt", replay["state"] == "completed")
        self.login("owner", test=TEST_ID)
        self.check("Clean removes test data and invalidates prior generation", self.cli("owner", "connection", "ls", test=TEST_ID) == [])
        self.check("Test clean leaves production intact", len(self.cli("owner", "connection", "ls")) >= 5)
        self.cli("owner", "config", "set", "telemetry", "false")
        self.check("Telemetry can be explicitly disabled", self.cli("owner", "config", "show")["telemetry"] is False)
        report = self.cli("owner", "report", "Fixture production-plane report with no mail configuration", expected=False)
        self.check("Missing Postmark configuration is not silent success", report["status"] == "delivery_failed" and bool(report["id"]))

    def close(self):
        for role in list(self.homes):
            try:
                self.cli(role, "daemon", "stop")
            except Exception:
                pass
        for child in reversed(self.children):
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for log in self.logfiles:
            log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--run-dir", type=Path)
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(["cargo", "build", "-p", "mcport-cli", "-p", "mcport-server"], cwd=ROOT, check=True)
    directory = args.run_dir or Path(tempfile.mkdtemp(prefix="mcport-e2e-"))
    directory.mkdir(parents=True, exist_ok=True)
    directory.chmod(0o700)
    journey = Journey(directory.resolve())
    try:
        journey.start()
        journey.run()
        result = {"passed": len(journey.checks), "checks": journey.checks, "run_dir": str(directory)}
        (directory / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
    finally:
        journey.close()


if __name__ == "__main__":
    main()
