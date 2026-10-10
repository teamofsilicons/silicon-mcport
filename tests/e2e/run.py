#!/usr/bin/env python3
"""Regression journey through the real MCPort binaries, a fake Silicon Accounts and MCP fixtures.

Starts tests/e2e/accounts_fake.py, then the MCP fixtures and mcport-server through scripts/dev_accounts.py (the same
code that runs MCPort against a real local Silicon Accounts stack), all on loopback ports: a free block of four by
default (website, service, providers, Silicon Accounts), or --base. Homes and data are fresh, under the run
directory. Never touches production Silicon Accounts, Postmark or Space Station, and prints only fixture data.

    python3 tests/e2e/run.py [--no-build] [--run-dir NEW_DIR] [--base PORT]

MCPORT_E2E_BASE sets the default base port (scripts/check.py passes the environment through).
"""
import argparse
import concurrent.futures
import html
import http.cookiejar
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import HTTPCookieProcessor, ProxyHandler, Request, build_opener

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).parent))
import directory_journey  # noqa: E402

OPENER = build_opener(ProxyHandler({}))
# Variables that would point the development stack somewhere else than the fake.
FOREIGN = ("ACCOUNTS_URL", "ACCOUNTS_API_URL", "MCPORT_APP_SECRET", "MCPORT_TEST_STACK", "MCPORT_DEV_DIR", "MCPORT_DEV_PIDS",
           "MCPORT_DEV_BASE", "MCPORT_URL", "MCPORT_SERVER_BIN", "POSTMARK_SERVER_TOKEN", "MCPORT_TELEMETRY_KEY", "SPACE_STATION_API_KEY")


def free_block(size=4):
    """The first port of `size` consecutive free loopback ports."""
    for _ in range(200):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            base = probe.getsockname()[1]
        if base + size > 65535:
            continue
        sockets = []
        try:
            for port in range(base, base + size):
                handle = socket.socket()
                sockets.append(handle)
                handle.bind(("127.0.0.1", port))
            return base
        except OSError:
            continue
        finally:
            for handle in sockets:
                handle.close()
    raise RuntimeError("No block of free loopback ports was found")


def request(url, body=None, method=None, headers=None, expected=200):
    data = json.dumps(body).encode() if body is not None else None
    options = {"Content-Type": "application/json", **(headers or {})}
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=150) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        status, raw = error.code, error.read()
    value = json.loads(raw) if raw else None
    if expected is not None and status != expected:
        raise AssertionError(f"HTTP {status}, expected {expected}, {url}: {value}")
    return value


class Journey:
    def __init__(self, directory, base):
        self.directory = directory
        self.base = base
        self.backend = f"http://127.0.0.1:{base + 1}"
        self.provider = f"http://127.0.0.1:{base + 2}"
        self.accounts = f"http://127.0.0.1:{base + 3}"
        self.children = []
        self.logfiles = []
        self.checks = []
        self.homes = {}
        self.who = {}
        target = Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target")
        target = target if target.is_absolute() else ROOT / target
        suffix = ".exe" if os.name == "nt" else ""
        self.cli_binary = str(target / "debug" / ("mcport" + suffix))
        self.server_binary = str(target / "debug" / ("mcport-server" + suffix))
        self.dev_env = {key: value for key, value in os.environ.items() if key not in FOREIGN}
        self.dev_env.update(MCPORT_TEST_STACK=str(directory / "accounts.json"), MCPORT_DEV_BASE=str(base),
                            MCPORT_DEV_DIR=str(directory / "dev"), MCPORT_SERVER_BIN=self.server_binary)

    # -- processes -----------------------------------------------------------------------------------------------

    def spawn(self, name, command):
        log = (self.directory / (name + ".log")).open("w")
        self.logfiles.append(log)
        process = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        self.children.append(process)
        return process

    def ready(self, url, process):
        for _ in range(200):
            if process.poll() is not None:
                raise AssertionError("A fixture exited before readiness; inspect " + str(self.directory))
            try:
                request(url)
                return
            except (URLError, OSError):
                time.sleep(0.1)
        raise AssertionError("Readiness timed out: " + url)

    def dev(self, command):
        result = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), command], env=self.dev_env,
                                capture_output=True, text=True, timeout=180)
        (self.directory / f"dev-accounts-{command}.log").write_text(result.stdout + result.stderr)
        if result.returncode != 0:
            raise AssertionError(f"dev_accounts.py {command} failed: {result.stderr}")
        return json.loads(result.stdout)

    def start(self):
        fake = self.spawn("accounts", [sys.executable, "-I", str(ROOT / "tests/e2e/accounts_fake.py"), "--port", str(self.base + 3),
                                       "--state", str(self.directory / "accounts.json")])
        self.ready(self.accounts + "/health", fake)
        started = self.dev("up")
        self.check("Service and providers start against Silicon Accounts; a signed test delivery is accepted",
                   started["webhook_test"]["outcome"] == "delivered" and started["backend_url"] == self.backend)

    def close(self):
        for role in list(self.homes):
            try:
                self.cli(role, "daemon", "stop", expected=None)
            except Exception:
                pass
        try:
            self.dev("down")
        except Exception as error:
            print(f"cleanup: {error}", flush=True)
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

    # -- identities ----------------------------------------------------------------------------------------------

    def fake(self, path, body=None, method=None, expected=200):
        return request(self.accounts + path, body, method=method, expected=expected)

    def account(self, role, kind, handle, custodian=None):
        body = {"kind": kind, "handle": handle, "display_name": handle.replace("-", " ").title()}
        if custodian:
            body["custodian"] = self.who[custodian]["uuid"]
        self.who[role] = self.fake("/fixture/accounts", body)
        return self.who[role]

    def home(self, role):
        if role not in self.homes:
            self.homes[role] = self.directory / "homes" / role
            self.homes[role].mkdir(parents=True)
        return self.homes[role]

    def env(self, role):
        home = str(self.home(role))
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": home, "SILICON_HOME": home, "LANG": "C",
               "MCPORT_URL": self.backend, "ACCOUNTS_URL": self.accounts}
        for key in ("SYSTEMROOT", "TEMP", "TMP"):  # Windows needs these to start processes
            if key in os.environ:
                env[key] = os.environ[key]
        return env

    def cli(self, role, *args, expected=True, stdin=None):
        command = [self.cli_binary, "--json", *args]
        result = subprocess.run(command, input=stdin, text=True, capture_output=True, env=self.env(role), timeout=160)
        try:
            value = json.loads(result.stdout.strip().splitlines()[-1]) if result.stdout.strip() else None
        except ValueError:
            raise AssertionError(f"CLI returned non-JSON: {args[:3]} {result.stdout} {result.stderr}") from None
        if expected is not None and (result.returncode == 0) != expected:
            safe_args = args[:1] if args and args[0] == "login" else args
            raise AssertionError(f"[{role}] {safe_args}: status {result.returncode}; {value}; stderr={result.stderr}")
        return value

    def check(self, label, condition=True, detail=None):
        if not condition:
            raise AssertionError(label + ("" if detail is None else f": {json.dumps(detail, default=str)[:1500]}"))
        self.checks.append(label)
        print("PASS " + label, flush=True)

    def login(self, role):
        """Silicons hand over a short-lived token; Carbons approve a device code."""
        who = self.who[role]
        if who["kind"] == "silicon":
            slt = self.fake("/fixture/slt", {"uuid": who["uuid"]})["slt"]
            result = self.cli(role, "login", "--slt-stdin", stdin=slt)
        else:
            process = subprocess.Popen([self.cli_binary, "--json", "login", "--label", "e2e " + role], env=self.env(role),
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                first = json.loads(process.stdout.readline())
                self.fake(f"/fixture/device/{first['user_code']}/approve", {"uuid": who["uuid"]})
                out, err = process.communicate(timeout=60)
            except BaseException:
                process.kill()
                raise
            if process.returncode != 0:
                raise AssertionError(f"[{role}] device sign-in failed: {out} {err}")
            result = json.loads(out.strip().splitlines()[-1])
        assert result["authenticated"] is True and result["uuid"] == who["uuid"], result
        return result

    def api_session(self, role="owner"):
        """Bearer headers for direct API calls: a fresh sign-in of `role` at the fake."""
        tokens = self.fake("/fixture/token", {"uuid": self.who[role]["uuid"]})
        return {"Authorization": "Bearer " + tokens["access_token"]}

    def access_token(self, role):
        files = sorted((self.home(role) / ".mcport" / "dir" / "accounts").glob("*.json"))
        assert len(files) == 1, files
        return files[0], json.loads(files[0].read_text())

    def wait(self, label, probe, timeout=20):
        deadline = time.time() + timeout
        while time.time() < deadline:
            value = probe()
            if value:
                return value
            time.sleep(0.2)
        raise AssertionError(label + ": timed out")

    def authority_index(self):
        # Read only non-secret record indexes. All authority is created/removed
        # through the real API; no ciphertext, master key or database writes.
        database = (self.directory / "dev" / "server" / "mcport.sqlite").as_uri() + "?mode=ro"
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

    # -- provider OAuth --------------------------------------------------------------------------------------------

    def oauth_consent(self, role, connection, account=None, lifetime=None):
        authorization = self.cli(role, "account", "connect", connection)
        browser = build_opener(ProxyHandler({}), HTTPCookieProcessor(http.cookiejar.CookieJar()))
        with browser.open(authorization["authorization_url"]) as response:
            consent = response.read().decode()
        provider_url = html.unescape(re.search(r"href=['\"]([^'\"]+)", consent).group(1))
        selections = {}
        if account is not None:
            selections["fixture_account"] = account
        if lifetime is not None:
            selections["fixture_token_lifetime"] = lifetime
        if selections:
            provider_url += ("&" if "?" in provider_url else "?") + urlencode(selections)
        with browser.open(provider_url) as response:
            assert response.status == 200

    def oauth_refresh_journey(self):
        def provider_activity():
            stats = request(self.provider + "/fixture/stats")
            return {"calls": stats["calls"], "oauth_exchanges": stats["oauth_exchanges"], "requests": [item for item in stats["requests"] if item["path"].startswith(("/mcp/", "/oauth/"))]}

        connection = "oauth-personal"
        self.cli("owner", "connection", "new", connection, "--transport", "http", "--url", self.provider + "/mcp/oauth", "--auth", "per-user", "--visibility", "circle")
        self.oauth_consent("owner", connection, "oauth-carbon", 20)
        before = provider_activity()
        self.cli("silicon", "tool", "call", connection, "whoami", expected=False)
        after = provider_activity()
        self.check("Missing personal OAuth grant never borrows the Carbon's grant or contacts the provider", before == after)

        def call(role, account, generation):
            result = self.cli(role, "tool", "call", connection, "whoami")["result"]["structuredContent"]
            assert result["account"] == account and result["token_generation"] == generation, result
            return result

        # Every issued token lives 20 seconds, inside the service's 30 second refresh window.
        # The provider revokes the old bearer and refresh token on each rotation.
        call("owner", "oauth-carbon", 1)
        call("owner", "oauth-carbon", 2)
        self.check("Short-lived OAuth refresh persists the rotated token and uses new bearers")
        self.oauth_consent("silicon", connection, "oauth-silicon", 20)
        call("silicon", "oauth-silicon", 1)
        call("owner", "oauth-carbon", 3)
        call("silicon", "oauth-silicon", 2)
        self.check("Carbon and Silicon personal OAuth refresh families remain isolated")

        self.cli("silicon", "account", "disconnect", connection)
        before = provider_activity()
        self.cli("silicon", "tool", "call", connection, "whoami", expected=False)
        after = provider_activity()
        self.check("Disconnected personal OAuth cannot refresh or fall back to the Carbon", before == after)
        call("owner", "oauth-carbon", 4)
        self.check("Disconnecting the Silicon's OAuth leaves the Carbon's account and rotation usable")

        events = [event for event in request(self.provider + "/fixture/stats")["oauth_exchanges"] if event["account"] in ("oauth-carbon", "oauth-silicon")]
        families = {}
        for account, maximum in (("oauth-carbon", 4), ("oauth-silicon", 2)):
            selected = [event for event in events if event["account"] == account]
            assert [event["generation"] for event in selected] == list(range(maximum + 1))
            assert [event["grant_type"] for event in selected] == ["authorization_code"] + ["refresh_token"] * maximum
            families[account] = {event["family"] for event in selected}
            assert len(families[account]) == 1
        assert families["oauth-carbon"].isdisjoint(families["oauth-silicon"])
        proof = {"access_token_lifetime_seconds": 20, "clock_waits": 0, "database_writes": False, "oauth_exchanges": events, "old_tokens_invalidated_on_refresh": True, "per_user_isolation": True, "missing_grant_no_fallback": True, "disconnect_denied_without_provider_contact": True}
        (self.directory / "oauth-refresh-proof.json").write_text(json.dumps(proof, indent=2))
        self.check("Provider recorded exactly six refresh rotations across two independent grants")

    # -- the journey -----------------------------------------------------------------------------------------------

    def run(self):
        discovery = self.cli("owner", "accounts")
        self.check("Discovery before sign-in names the app and its Silicon Accounts",
                   discovery["app_id"] == "mcport" and discovery["accounts_url"] == self.accounts)
        self.account("owner", "carbon", "owner")
        self.account("silicon", "silicon", "researcher", custodian="owner")
        self.account("stranger", "carbon", "stranger")
        self.account("outsider", "carbon", "outsider")
        for role in ("owner", "silicon", "stranger", "outsider"):
            self.login(role)
            self.check("Signed in: " + role, self.cli(role, "login", "status")["verified"] is True)
        directory_journey.run(self, request)
        self.connections_and_tools()
        self.provider_accounts()
        self.oauth_refresh_journey()
        self.connection_deletion()
        self.local_hosts()
        self.idempotency_and_cancellation()
        self.accounts_events()
        self.operations()

    def connections_and_tools(self):
        self.cli("owner", "connection", "new", "public", "--transport", "http", "--url", self.provider + "/mcp/public", "--auth", "none", "--visibility", "private")
        self.check("A Carbon creates a connection it owns")
        self.check("An invite-only connection without grants is hidden from another Carbon", not self.cli("stranger", "connection", "ls"))
        self.cli("stranger", "tool", "ls", "public", expected=False)
        self.check("…and another Carbon cannot use it")
        self.cli("owner", "connection", "set", "public", "--visibility", "circle")
        seen = next((c for c in self.cli("silicon", "connection", "ls") if c["name"] == "public"), None)
        self.check("A circle connection reaches the Silicon its owner looks after", seen is not None and seen["access"] == "circle")
        self.cli("stranger", "tool", "ls", "public", expected=False)
        self.check("A circle connection does not reach other Carbons")
        self.cli("owner", "connection", "set", "public", "--visibility", "invited")
        self.cli("owner", "access", "new", "public", "--account", "si:researcher")
        self.cli("owner", "access", "new", "public", "--account", "c:stranger")
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
        self.nested_call = nested_call
        nested = nested_call["result"]
        self.check("Text, image, resource-link and structured output preserved", len(nested["content"]) == 3 and nested["structuredContent"]["arguments"]["options"]["tags"] == ["a", "b"])
        assets = self.cli("silicon", "asset", "ls", nested_call["call_id"])
        image_asset = next(asset for asset in assets if asset["mime_type"] == "image/png")
        saved = self.directory / "fixture-image.png"
        self.cli("silicon", "asset", "get", nested_call["call_id"], str(image_asset["index"]), "--output", str(saved))
        original = saved.read_bytes()
        self.check("Authorized embedded asset saves privately", bool(original) and (os.name != "posix" or saved.stat().st_mode & 0o777 == 0o600))
        self.cli("silicon", "asset", "get", nested_call["call_id"], str(image_asset["index"]), "--output", str(saved), expected=False)
        self.check("Asset output never overwrites an existing file", saved.read_bytes() == original)
        self.check("The custodian sees the assets of its Silicon's call", bool(self.cli("owner", "asset", "ls", nested_call["call_id"])))
        stranger_call = self.cli("stranger", "tool", "call", "public", "nested", "--input", '{"options":{"tags":["c"]}}')
        self.cli("owner", "asset", "ls", stranger_call["call_id"], expected=False)
        self.check("Connection ownership does not reveal another caller's assets")
        link = self.cli("silicon", "asset", "link", nested_call["call_id"], str(image_asset["index"]))
        with OPENER.open(link["url"], timeout=30) as response:
            downloaded = response.read()
        try:
            OPENER.open(link["url"], timeout=30)
            reused = 200
        except HTTPError as error:
            reused = error.code
        self.check("A one-time download link works once without a token", downloaded == original and reused == 404)
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
        self.cli("owner", "tool", "set", "public", "echo", "--account", "si:researcher", "--enabled", "false")
        self.cli("silicon", "tool", "call", "public", "echo", "--input", '{"message":"denied"}', expected=False)
        self.cli("owner", "tool", "call", "public", "echo", "--input", '{"message":"owner allowed"}')
        self.check("Per-account disabled tool leaves the owner's access intact")
        self.cli("owner", "tool", "set", "public", "echo", "--enabled", "false")
        self.cli("owner", "tool", "set", "public", "echo", "--account", "si:researcher", "--enabled", "true")
        self.cli("silicon", "tool", "call", "public", "echo", "--input", '{"message":"denied"}', expected=False)
        self.check("Per-account enable cannot bypass a connection-wide disable")
        self.cli("owner", "tool", "set", "public", "echo", "--enabled", "true")

    def provider_accounts(self):
        for name, mode in (("shared", "shared"), ("personal", "per-user")):
            self.cli("owner", "connection", "new", name, "--transport", "http", "--url", self.provider + "/mcp/bearer", "--auth", mode, "--visibility", "circle")
            self.cli("owner", "account", "connect", name, "--input", '{"kind":"bearer","secret":"fixture-owner-token","label":"Owner fixture provider"}')
        shared = self.cli("silicon", "tool", "call", "shared", "whoami")
        self.check("The Silicon deliberately uses its custodian's shared provider account", shared["result"]["structuredContent"]["account"] == "provider-owner")
        self.cli("silicon", "tool", "call", "personal", "whoami", expected=False)
        self.check("A missing personal account never falls back to the owner's")
        self.cli("silicon", "account", "connect", "personal", "--input", '{"kind":"bearer","secret":"fixture-silicon-token","label":"Silicon fixture provider"}')
        self.check("Personal account isolation", self.cli("silicon", "tool", "call", "personal", "whoami")["result"]["structuredContent"]["account"] == "provider-silicon")
        inspected = self.cli("owner", "account", "show", "personal", "--account", "si:researcher")
        self.check("The custodian inspects its Silicon's personal account without seeing the secret",
                   inspected["connected"] is True and "fixture-silicon-token" not in json.dumps(inspected))
        self.cli("owner", "account", "disconnect", "personal", "--account", "si:researcher")
        self.cli("silicon", "tool", "call", "personal", "whoami", expected=False)
        self.check("The custodian's disconnect immediately blocks further personal use")
        self.cli("owner", "connection", "new", "oauth", "--transport", "http", "--url", self.provider + "/mcp/oauth", "--auth", "shared", "--visibility", "circle")
        self.oauth_consent("owner", "oauth")
        self.check("OAuth PKCE consent binds the creator's account for its Silicon", self.cli("silicon", "tool", "call", "oauth", "whoami")["result"]["structuredContent"]["account"] == "oauth-owner")

    def connection_deletion(self):
        authority_before = self.authority_index()
        self.cli("owner", "connection", "new", "delete-authority", "--transport", "http", "--url", self.provider + "/mcp/oauth", "--auth", "per-user", "--visibility", "invited")
        self.cli("owner", "access", "new", "delete-authority", "--account", "si:researcher")
        self.cli("owner", "tool", "set", "delete-authority", "echo", "--enabled", "false")
        for role, token in (("owner", "fixture-owner-token"), ("silicon", "fixture-silicon-token")):
            self.cli(role, "account", "connect", "delete-authority", "--input", json.dumps({"kind": "bearer", "secret": token}))
        self.cli("silicon", "account", "connect", "delete-authority")
        created_authority = self.authority_index() - authority_before
        assert {kind for kind, _ in created_authority} == {"connection", "credential", "account_epoch", "grant", "policy", "oauth_attempt"}, created_authority
        assert sum(kind == "credential" for kind, _ in created_authority) == 2
        self.cli("owner", "connection", "rm", "delete-authority")
        self.check("Connection deletion removes both personal accounts, policy, grant and pending OAuth authority", not (created_authority & self.authority_index()))
        self.cli("silicon", "account", "show", "delete-authority", expected=False)
        self.check("Connection deletion preserves another connection's shared account", self.cli("silicon", "tool", "call", "shared", "whoami")["result"]["structuredContent"]["account"] == "provider-owner")

    def local_hosts(self):
        host = self.cli("owner", "host", "new", "laptop")
        self.check("A host registers and its daemon keys accounts by uuid (registry version 2)",
                   '"registry_version": 2' in json.dumps(self.cli("owner", "daemon", "status")) and host["host"]["name"] == "laptop")
        self.cli("owner", "connection", "new", "desktop", "--host", "laptop", "--transport", "http", "--url", self.provider + "/mcp/local", "--auth", "shared", "--visibility", "circle")
        self.wait_connection("desktop")
        self.check("The Silicon invokes a registered local HTTP MCP on its custodian's machine", self.cli("silicon", "tool", "call", "desktop", "whoami")["result"]["structuredContent"]["account"] == "desktop-owner")
        self.cli("owner", "connection", "new", "stdio", "--host", "laptop", "--transport", "stdio", "--command", sys.executable, "--arg", str(ROOT / "tests/e2e/fixtures.py"), "--arg=--stdio", "--env", "FIXTURE_ACCOUNT=stdio-isolated", "--auth", "none", "--visibility", "circle")
        self.wait_connection("stdio")
        self.check("The Silicon invokes an allowlisted local stdio process", self.cli("silicon", "tool", "call", "stdio", "whoami")["result"]["structuredContent"]["account"] == "stdio-isolated")
        self.cli("owner", "connection", "new", "not-mcp", "--host", "laptop", "--transport", "http", "--url", self.provider + "/not-an-mcp", "--auth", "none", "--visibility", "circle")
        self.wait_status("not-mcp", "offline")
        self.check("An open HTTP port without MCP is offline while its host remains online", self.cli("owner", "host", "show", "laptop")["online"])
        missing = str((self.directory / "missing-mcp-program").resolve())
        self.cli("owner", "connection", "new", "missing-program", "--host", "laptop", "--transport", "stdio", "--command", missing, "--auth", "none", "--visibility", "circle")
        self.wait_status("missing-program", "offline")
        self.check("A missing stdio executable is offline while working connections still run", self.cli("silicon", "tool", "call", "desktop", "whoami")["result"]["structuredContent"]["account"] == "desktop-owner")
        self.cli("outsider", "tool", "call", "desktop", "whoami", expected=False)
        self.check("An unrelated Carbon cannot reach the circle's local connection")
        self.cli("owner", "account", "disconnect", "desktop")
        self.cli("silicon", "tool", "call", "desktop", "whoami", expected=False)
        self.check("Local shared disconnect cannot fall back to the desktop account")
        self.cli("owner", "account", "connect", "desktop")
        self.wait_connection("desktop")
        self.cli("owner", "daemon", "stop")
        self.wait_connection("desktop", ready=False)
        self.cli("silicon", "tool", "call", "desktop", "whoami", expected=False)
        self.check("An offline local host never silently reroutes")
        self.cli("owner", "daemon", "start")
        self.wait_connection("desktop")
        self.cli("silicon", "tool", "call", "desktop", "whoami")
        self.check("Local host reconnect recovers service")

    def idempotency_and_cancellation(self):
        headers = self.api_session("owner")
        endpoint = self.backend + "/api/v1/connections/public/mcp"
        rpc = {"method": "tools/call", "params": {"name": "write", "arguments": {"value": "single logical write"}}, "idempotency_key": "fixture-idempotency-key"}
        first = request(endpoint, rpc, headers=headers)["data"]
        second = request(endpoint, rpc, headers=headers)["data"]
        calls = request(self.provider + "/fixture/stats")["calls"]
        self.check("Same idempotency key returns the same result without replay", first == second and sum(call["tool"] == "write" for call in calls) == 1)
        rpc["params"]["arguments"]["value"] = "changed body"
        self.check("Idempotency key rejects a changed request", request(endpoint, rpc, headers=headers, expected=409)["error"]["code"] == "idempotency_conflict")
        dropped = self.cli("owner", "tool", "call", "public", "drop", "--input", '{"value":"do not replay"}', expected=False)
        calls = request(self.provider + "/fixture/stats")["calls"]
        self.check("Lost upstream response reports unknown outcome with one dispatch", dropped["error"]["outcome_unknown"] and sum(call["tool"] == "drop" for call in calls) == 1)
        with concurrent.futures.ThreadPoolExecutor() as workers:
            pending = workers.submit(request, endpoint, {"method": "tools/call", "params": {"name": "slow", "arguments": {"seconds": 4}}, "idempotency_key": "fixture-cancel-key"}, None, headers, None)
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
        status = request(self.backend + "/api/v1/connections", headers={"Authorization": "Proof sap_fixture"}, expected=401)
        self.check("Proofs from other apps are refused (proof_not_accepted)", status["error"]["code"] == "proof_not_accepted")
        self.cli("owner", "access", "rm", "public", "--account", "si:researcher")
        self.cli("silicon", "tool", "ls", "public", expected=False)
        self.check("Invitation revocation defeats cached discovery")
        revoked_result = self.cli("silicon", "activity", "show", self.nested_call["call_id"], expected=False)
        self.check("Invitation revocation also blocks full-result GET", "error" in revoked_result and "result" not in revoked_result)

    def accounts_events(self):
        """What Silicon Accounts tells MCPort through its webhook, and the rules that rest on it."""
        silicon = self.who["silicon"]
        notes = self.cli("silicon", "connection", "new", "silicon-notes", "--transport", "http", "--url", self.provider + "/mcp/public", "--auth", "none")

        def owner_of(role):
            return self.cli(role, "connection", "show", notes["id"])["owner"]

        self.fake(f"/fixture/accounts/{silicon['uuid']}/id", {"id": "si:researcher-two"})
        self.check("account.id_changed: the new id shows at once; the uuid stays",
                   owner_of("owner")["id"] == "si:researcher-two" and self.cli("silicon", "login", "status")["id"] == "si:researcher-two")
        self.fake(f"/fixture/accounts/{silicon['uuid']}/rename", {"display_name": "Researcher Renamed"})
        self.check("account.updated: the new display name shows", owner_of("owner")["display_name"] == "Researcher Renamed")
        replayed = self.fake("/fixture/replay-last", {"type": "account.id_changed"})
        forged = request(self.backend + "/webhooks/accounts", {"event_id": "forged", "type": "ping", "occurred_at": "2026-10-10T00:00:00.000Z", "app_id": "mcport", "silicon": None, "data": {}},
                         headers={"X-Accounts-Timestamp": str(int(time.time())), "X-Accounts-Signature": "v1=" + "0" * 64}, expected=401)
        self.check("A replayed event_id is acknowledged as a duplicate; a forged delivery is refused",
                   json.loads(replayed["attempts"][-1]["body"]) == {"received": True, "duplicate": True}
                   and forged["error"]["code"] == "invalid_webhook_signature" and owner_of("owner")["id"] == "si:researcher-two",
                   {"replayed": replayed["attempts"], "forged": forged})

        self.cli("outsider", "connection", "new", "outside-tools", "--transport", "http", "--url", self.provider + "/mcp/public", "--auth", "none")
        refused = self.cli("outsider", "access", "new", "outside-tools", "--account", "si:researcher-two", expected=False)
        self.check("Silicons are not open to the world: an unrelated Carbon cannot share with one", refused["error"]["code"] == "silicon_not_reachable")
        self.cli("owner", "allow", "add", "c:outsider", "--silicon", "si:researcher-two")
        self.cli("outsider", "access", "new", "outside-tools", "--account", "si:researcher-two")
        seen = next((c for c in self.cli("silicon", "connection", "ls") if c["name"] == "outside-tools"), None)
        self.check("After its custodian allows that Carbon, the share reaches the Silicon", seen is not None and seen["access"] == "invited")
        self.cli("silicon", "allow", "rm", "c:outsider")
        self.cli("outsider", "connection", "new", "outside-more", "--transport", "http", "--url", self.provider + "/mcp/public", "--auth", "none")
        again = self.cli("outsider", "access", "new", "outside-more", "--account", "si:researcher-two", expected=False)
        self.check("The Silicon removes the allowance; new shares are refused again", again["error"]["code"] == "silicon_not_reachable")

        self.fake(f"/fixture/accounts/{silicon['uuid']}/remove-app", {})
        ended = self.cli("silicon", "connection", "ls", expected=False)
        self.check("membership.access_removed: the Silicon's sign-in ends at once", ended["error"]["code"] == "sign_in_ended")
        self.check("…its custodian still sees its connection while its access is removed", owner_of("owner")["uuid"] == silicon["uuid"])
        self.login("silicon")
        self.check("Signing in again restores the Silicon's access", any(c["id"] == notes["id"] for c in self.cli("silicon", "connection", "ls")))
        self.fake(f"/fixture/accounts/{silicon['uuid']}/stk", {})
        ended = self.cli("silicon", "connection", "ls", expected=False)
        self.check("membership.signed_out (stk_rotated): the Silicon's sign-in ends", ended["error"]["code"] == "sign_in_ended")
        self.login("silicon")

        path, stored = self.access_token("outsider")
        stolen = self.fake("/v1/oauth/token", {"grant_type": "refresh_token", "refresh_token": stored["refresh_token"], "client_id": "mcport"})
        stored["expires_at"] = int(time.time()) + 30
        path.write_text(json.dumps(stored))
        path.chmod(0o600)
        ended = self.cli("outsider", "connection", "ls", expected=False)
        thief = request(self.backend + "/api/v1/connections", headers={"Authorization": "Bearer " + stolen["access_token"]}, expected=401)
        self.check("A refresh token used twice ends the sign-in (refresh_token_reuse); the copy's access token is refused",
                   ended["error"]["code"] == "sign_in_ended" and thief["error"]["code"] == "signed_out")
        self.login("outsider")

        self.fake(f"/fixture/accounts/{silicon['uuid']}/transfer", {"to": self.who["stranger"]["uuid"]})
        moved = self.cli("stranger", "connection", "show", notes["id"])
        self.check("silicon.custodian_changed: the new custodian manages the Silicon's connection", moved["access"] == "custodian" and moved["can_manage"] is True)
        self.cli("owner", "connection", "show", notes["id"], expected=False)
        self.check("…the previous custodian loses it, and the Silicon leaves its circle",
                   not any(c["name"] == "shared" for c in self.cli("silicon", "connection", "ls")))
        token = self.access_token("silicon")[1]["access_token"]
        self.fake(f"/fixture/accounts/{silicon['uuid']}", {}, method="DELETE")
        gone = request(self.backend + "/api/v1/connections", headers={"Authorization": "Bearer " + token}, expected=401)
        self.cli("stranger", "connection", "show", notes["id"], expected=False)
        grants = self.cli("outsider", "access", "ls", "outside-tools")
        self.check("account.deleted: the Silicon's tokens are refused, its connections and the shares to it are gone",
                   gone["error"]["code"] == "account_deleted" and grants == [])

    def operations(self):
        self.cli("owner", "config", "set", "telemetry", "false")
        self.check("Telemetry can be explicitly disabled", self.cli("owner", "config", "show")["telemetry"] is False)
        report = self.cli("owner", "report", "Fixture report with no mail configuration", expected=False)
        self.check("Missing Postmark configuration is not silent success", report["status"] == "delivery_failed" and bool(report["id"]))
        database = (self.directory / "dev" / "server" / "mcport.sqlite").as_uri() + "?mode=ro"
        with sqlite3.connect(database, uri=True) as connection:
            resources = connection.execute("SELECT kind,id FROM records WHERE kind IN ('connection','host','call','report')").fetchall()
        self.check("All public resource creation paths use globally unique three-character base62 IDs", {
            kind for kind, _ in resources
        } == {"connection", "host", "call", "report"} and all(
            re.fullmatch(r"[a-z0-9A-Z]{3}", identifier) for _, identifier in resources
        ) and len({identifier for _, identifier in resources}) == len(resources))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--run-dir", type=Path)
    parser.add_argument("--base", type=int, default=int(os.environ["MCPORT_E2E_BASE"]) if os.environ.get("MCPORT_E2E_BASE") else None,
                        help="first of four loopback ports: website (unused), service, providers, Silicon Accounts (default: MCPORT_E2E_BASE, else a free block)")
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(["cargo", "build", "--locked", "-p", "mcport-cli", "-p", "mcport-server"], cwd=ROOT, check=True)
    directory = args.run_dir or Path(tempfile.mkdtemp(prefix="mcport-e2e-"))
    directory.mkdir(parents=True, exist_ok=True)
    directory.chmod(0o700)
    journey = Journey(directory.resolve(), args.base or free_block())
    try:
        journey.start()
        journey.run()
        result = {"passed": len(journey.checks), "checks": journey.checks, "run_dir": str(directory)}
        (directory / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({"passed": result["passed"], "run_dir": result["run_dir"]}))
    finally:
        journey.close()


if __name__ == "__main__":
    main()
