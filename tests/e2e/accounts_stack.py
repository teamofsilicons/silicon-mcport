#!/usr/bin/env python3
"""End-to-end scenarios against a local Silicon Accounts stack, with real tokens and the real binaries.

    MCPORT_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \\
      scripts/e2e-accounts.sh [--only 1,2,…] [--keep-running]

Starts the development stack (scripts/dev_accounts.py up) unless it already runs, then:
  1. a Carbon signs in on the hosted pages and drives the API (create, list, read, update, use, delete);
  2. a Silicon signs the CLI in with a short-lived token and works with connections, tools and activity;
  3. a Carbon signs the CLI in with the device flow;
  4. custodian rule, the Silicons it looks after, sharing by id and "Silicons are not open to the world";
  5. webhooks: id changes, a replayed event, forged deliveries, a Silicon removing MCPort;
  6. proofs from another app are refused (MCPort honours none yet);
  7. the three Silicon Apps discovery commands from a freshly packed archive, in an empty home;
  8. restart safety: stored sign-ins and webhook dedupe survive a service restart.
Every identity is new per run (`mcport-e2e-*-<run>`). Results go to <dev dir>/e2e/run-<run>/ (result.json and
transcript.jsonl, never tokens); every sign-in made here is signed out at the end, and whatever this run started
is stopped unless --keep-running.

Environment: everything scripts/dev_accounts.py reads, plus
  SILICON_ACCOUNTS_DIR   a silicon-accounts checkout with testkit dependencies installed (for tests/e2e/stack/mint.mts)
  SILICON_ACCOUNTS_CLI   the silicon-accounts CLI built from it (default $SILICON_ACCOUNTS_DIR/target/debug/silicon-accounts)
  MCPORT_CLI_BIN         the mcport CLI (default $CARGO_TARGET_DIR/debug/mcport, else target/debug/mcport)
  MCPORT_E2E_TSX / MCPORT_E2E_MINT   override the TypeScript runner / the minting script
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import time
import tomllib
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import dev_accounts  # noqa: E402  (shared configuration and HTTP helper)

http = dev_accounts.http


class Check(AssertionError):
    pass


class Run:
    def __init__(self, args):
        self.cfg = dev_accounts.load_config()
        self.args = args
        self.n = str(int(time.time()))
        self.dir = self.cfg["dir"] / "e2e" / f"run-{self.n}"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.transcript = (self.dir / "transcript.jsonl").open("a")
        self.checks = []
        self.started_stack = False
        self.homes = {}
        self.revoke_later = []  # (app_id, secret, refresh token) of API sign-ins made here
        env = os.environ.get
        target = Path(env("CARGO_TARGET_DIR") or ROOT / "target")
        target = target if target.is_absolute() else ROOT / target
        self.cli_bin = Path(env("MCPORT_CLI_BIN") or target / "debug" / "mcport")
        sa_dir = env("SILICON_ACCOUNTS_DIR")
        if not sa_dir:
            raise SystemExit("e2e-accounts: set SILICON_ACCOUNTS_DIR to a silicon-accounts checkout (its testkit mints identities).")
        self.sa_dir = Path(sa_dir)
        self.tsx = env("MCPORT_E2E_TSX") or str(self.sa_dir / "testkit/node_modules/.bin/tsx")
        self.mint_script = env("MCPORT_E2E_MINT") or str(ROOT / "tests/e2e/stack/mint.mts")
        self.sa_cli = env("SILICON_ACCOUNTS_CLI") or str(self.sa_dir / "target/debug/silicon-accounts")
        stack_file = env("MCPORT_TEST_STACK")
        self.stack = json.loads(Path(stack_file).read_text()) if stack_file else {}
        for path, what in ((self.cli_bin, "MCPORT_CLI_BIN"), (Path(self.tsx), "MCPORT_E2E_TSX"), (Path(self.sa_cli), "SILICON_ACCOUNTS_CLI")):
            if not path.exists():
                raise SystemExit(f"e2e-accounts: {path} does not exist (set {what}).")
        self.email = {name: f"mcport-e2e-{name}-{self.n}@example.test" for name in ("c1", "c2")}
        self.handle = {name: f"mcport-e2e-{name}-{self.n}" for name in ("s1", "s2")}
        self.who = {}

    # -- recording ---------------------------------------------------------------------------------------------

    def log(self, kind, **entry):
        self.transcript.write(json.dumps({"at": time.strftime("%H:%M:%S"), "kind": kind, **entry}) + "\n")
        self.transcript.flush()

    def check(self, label, condition=True, detail=None):
        if not condition:
            self.log("fail", label=label, detail=detail)
            raise Check(f"{label}" + (f": {json.dumps(detail)[:1500]}" if detail is not None else ""))
        self.checks.append(label)
        self.log("pass", label=label)
        print(f"PASS {label}", flush=True)

    # -- tools -------------------------------------------------------------------------------------------------

    def mint(self, *args):
        """One mint.mts command (never logged: its output carries tokens). Retries once on the stack's code limits."""
        env = dict(os.environ, SILICON_ACCOUNTS_DIR=str(self.sa_dir), ACCOUNTS_URL=self.cfg["accounts_api_url"])
        for attempt in (1, 2):
            result = subprocess.run([self.tsx, self.mint_script, *args], capture_output=True, text=True, env=env, timeout=180)
            if result.returncode == 0:
                self.log("mint", command=args[0], ok=True)
                return json.loads(result.stdout.strip().splitlines()[-1])
            limited = "too many" in result.stderr.lower() or "rate" in result.stderr.lower()
            self.log("mint", command=args[0], ok=False, stderr=result.stderr[-600:])
            if attempt == 1 and limited:
                print("  (the stack's code limit answered; waiting 40 s for its janitor)", flush=True)
                time.sleep(40)
                continue
            raise Check(f"mint {args[0]} failed: {result.stderr[-800:]}")

    def home(self, name):
        if name not in self.homes:
            path = self.dir / "homes" / name
            path.mkdir(parents=True, exist_ok=True)
            self.homes[name] = path
        return self.homes[name]

    def cli_env(self, home):
        return {"PATH": "/usr/bin:/bin", "HOME": str(home), "SILICON_HOME": str(home), "LANG": "C",
                "MCPORT_URL": self.cfg["backend_url"], "ACCOUNTS_URL": self.cfg["accounts_url"]}

    def mc(self, name, *args, ok=True, stdin=None, raw=False, timeout=180):
        """Run `mcport --json …` in identity `name`'s own home. ok=True/False asserts the exit status."""
        home = self.home(name)
        result = subprocess.run([str(self.cli_bin), "--json", *args], input=stdin, capture_output=True, text=True,
                                env=self.cli_env(home), timeout=timeout)
        shown = list(args)
        self.log("cli", who=name, args=shown, exit=result.returncode, stdout=result.stdout[-3000:], stderr=result.stderr[-800:])
        if ok is not None and (result.returncode == 0) != ok:
            raise Check(f"[{name}] mcport {' '.join(shown)} exited {result.returncode}: {result.stdout[-800:]} {result.stderr[-400:]}")
        if raw:
            return result
        try:
            return json.loads(result.stdout.strip().splitlines()[-1]) if result.stdout.strip() else None
        except ValueError:
            raise Check(f"[{name}] mcport {' '.join(shown)} printed non-JSON: {result.stdout[-400:]}") from None

    def api(self, method, path, token=None, body=None, scheme="Bearer", expect=None, base=None):
        url = (base or self.cfg["backend_url"]) + path
        headers = {"Authorization": f"{scheme} {token}"} if token else {}
        status, value = http(method, url, body=body, headers=headers, timeout=60)
        self.log("api", method=method, path=path, scheme=scheme if token else None, status=status, body=value)
        if expect is not None and status != expect:
            raise Check(f"{method} {path}: HTTP {status}, expected {expect}: {json.dumps(value)[:800]}")
        return status, value

    def accounts_app(self, method, path, body=None, app="mcport", expect=None):
        secret = self.cfg["app_secret"] if app == "mcport" else self.stack.get("apps", {}).get(app, {}).get("app_secret")
        status, value = http(method, self.cfg["accounts_api_url"] + path, body=body, basic=(app, secret),
                             headers={"Idempotency-Key": str(uuid.uuid4())} if method != "GET" else None)
        if expect is not None and status not in (expect if isinstance(expect, tuple) else (expect,)):
            raise Check(f"Silicon Accounts {method} {path} as {app}: HTTP {status}: {json.dumps(value)[:600]}")
        return status, value

    def first_party(self, method, path, token, body=None, expect=None):
        status, value = http(method, self.cfg["accounts_api_url"] + path, body=body, bearer=token,
                             headers={"Idempotency-Key": str(uuid.uuid4())} if method != "GET" else None)
        self.log("accounts", method=method, path=path, status=status)
        if expect is not None and status not in (expect if isinstance(expect, tuple) else (expect,)):
            raise Check(f"Silicon Accounts {method} {path}: HTTP {status}: {json.dumps(value)[:600]}")
        return status, value

    def sa(self, home_name, *args, stdin=None, ok=True):
        """The silicon-accounts CLI with its own home (never the machine's signed-in one)."""
        home = self.home("accounts-" + home_name)
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "ACCOUNTS_HOME": str(home), "LANG": "C"}
        result = subprocess.run([self.sa_cli, "--url", self.cfg["accounts_url"], "--home", str(home), *args],
                                input=stdin, capture_output=True, text=True, env=env, timeout=120)
        self.log("silicon-accounts", who=home_name, args=[a for a in args if not a.startswith("slt_")],
                 exit=result.returncode, stderr=result.stderr[-600:])
        if ok and result.returncode != 0:
            raise Check(f"silicon-accounts {' '.join(args)} exited {result.returncode}: {result.stdout[-400:]} {result.stderr[-600:]}")
        return result

    def wait(self, label, probe, timeout=25, interval=0.5):
        """Poll probe() until it returns a truthy value; fail with the last value after `timeout` seconds."""
        deadline, last = time.time() + timeout, None
        while time.time() < deadline:
            last = probe()
            if last:
                return last
            time.sleep(interval)
        raise Check(f"{label}: not within {timeout} s (last: {json.dumps(last, default=str)[:600]})")

    def sign_webhook(self, body, timestamp=None, secret=None):
        raw = json.dumps(body, separators=(",", ":")).encode()
        timestamp = str(timestamp or int(time.time()))
        key = (secret or dev_accounts.read_secret(self.cfg)).encode()
        signature = hmac.new(key, timestamp.encode() + b"." + raw, hashlib.sha256).hexdigest()
        return raw, {"X-Accounts-Timestamp": timestamp, "X-Accounts-Signature": "v1=" + signature,
                     "X-Accounts-Event-Id": body["event_id"], "X-Accounts-Event-Type": body["type"]}

    def deliver(self, raw, headers):
        status, value = dev_accounts.http_raw("POST", self.cfg["backend_url"] + "/webhooks/accounts", raw, headers)
        self.log("webhook", event=headers.get("X-Accounts-Event-Id"), status=status, body=value)
        return status, value

    # -- scenario 1: a Carbon on the API -----------------------------------------------------------------------

    def scenario_1(self):
        redirect = f"http://localhost:{self.cfg['base']}/auth/callback"
        tokens = self.mint("app-signin", "--app", "mcport", "--email", self.email["c1"], "--redirect", redirect, "--exchange")["tokens"]
        account = tokens["account"]
        token = tokens["access_token"]
        self.revoke_later.append(("mcport", self.cfg["app_secret"], tokens["refresh_token"]))
        self.who["c1"] = {"uuid": account["uuid"], "id": account["id"], "api_token": token}
        self.check("Carbon signed in on the hosted pages; the code exchange returned an mcport access token",
                   account["kind"] == "carbon" and account["id"] == "c:" + f"mcport-e2e-c1-{self.n}" and token.count(".") == 2,
                   {"account": {k: account.get(k) for k in ("uuid", "id", "kind")}})
        _, me = self.api("GET", "/api/v1/me", token, expect=200)
        self.check("GET /api/v1/me names the Carbon by uuid and current id",
                   me["data"]["uuid"] == account["uuid"] and me["data"]["id"] == account["id"] and me["data"]["kind"] == "carbon", me)
        self.check("…with the display name and photo the Carbon shares with MCPort (from the user base)",
                   me["data"]["display_name"] == account["display_name"] and me["data"].get("pfp_url") == account["pfp_url"], me)
        status, body = self.api("GET", "/api/v1/connections")
        self.check("No bearer token: 401 authentication_required", status == 401 and body["error"]["code"] == "authentication_required", body)
        status, body = self.api("GET", "/api/v1/connections", token[:-4] + ("AAAA" if not token.endswith("AAAA") else "BBBB"))
        self.check("A token with a broken signature: 401", status == 401 and body["error"]["code"] in ("invalid_token", "token_expired"), body)

        created = self.api("POST", "/api/v1/connections", token, {
            "name": "api-notes", "description": "Made through the API", "transport": "http",
            "url": self.cfg["providers_url"] + "/mcp/public", "auth_mode": "none"}, expect=200)[1]["data"]
        self.check("Create: the Carbon owns the new connection (invited, owner access)",
                   created["owner"]["uuid"] == account["uuid"] and created["access"] == "owner" and created["visibility"] == "invited", created)
        listed = self.api("GET", "/api/v1/connections", token, expect=200)[1]["data"]
        self.check("List: the connection is listed for its owner", [c["id"] for c in listed if c["id"] == created["id"]] == [created["id"]])
        read = self.api("GET", f"/api/v1/connections/{created['id']}", token, expect=200)[1]["data"]
        self.check("Read: the connection by id", read["name"] == "api-notes" and read["can_manage"] is True, read)
        updated = self.api("PATCH", f"/api/v1/connections/{created['id']}", token,
                           {"description": "Updated through the API", "version": read["version"]}, expect=200)[1]["data"]
        self.check("Update (live route): description changed and version moved on",
                   updated["description"] == "Updated through the API" and updated["version"] > read["version"], updated)
        stale = self.api("PATCH", f"/api/v1/connections/{created['id']}", token, {"description": "stale", "version": read["version"]})
        self.check("Update with a stale version is refused (409)", stale[0] == 409, stale[1])
        call = self.api("POST", f"/api/v1/connections/{created['id']}/mcp", token,
                        {"method": "tools/call", "params": {"name": "echo", "arguments": {"message": "from the API"}}}, expect=200)[1]["data"]
        self.check("Use: a tool call through the API runs and is recorded",
                   call["result"]["structuredContent"]["arguments"]["message"] == "from the API" and call["call_id"], call)
        history = self.api("GET", f"/api/v1/calls/{call['call_id']}", token, expect=200)[1]["data"]
        self.check("The call is in the Carbon's activity, caller = the Carbon",
                   history["caller"]["uuid"] == account["uuid"] and history["status"] == "completed", history)
        self.api("DELETE", f"/api/v1/connections/{created['id']}", token, expect=200)
        gone = self.api("GET", f"/api/v1/connections/{created['id']}", token)
        self.check("Delete: the connection is gone (404 not_found)", gone[0] == 404 and gone[1]["error"]["code"] == "not_found", gone[1])

    # -- scenario 2: a Silicon on the CLI ----------------------------------------------------------------------

    def silicon_login(self, name, slt):
        login = self.mc(name, "login", "--slt-stdin", stdin=slt)
        return login

    def session_file(self, name):
        files = sorted((self.home(name) / ".mcport" / "dir" / "accounts").glob("*.json"))
        if len(files) != 1:
            raise Check(f"expected one sign-in file in {name}'s home, found {[f.name for f in files]}")
        return files[0]

    def assets_and_tickets(self, name, connection):
        call = self.mc(name, "tool", "call", connection, "nested", "--input", '{"options":{"tags":["a","b"]}}')
        assets = self.mc(name, "asset", "ls", call["call_id"])
        image = next((a for a in assets if a["mime_type"] == "image/png"), None)
        self.check("asset ls lists the image a call returned", image is not None, assets)
        saved = self.dir / f"{name}-pixel.png"
        self.mc(name, "asset", "get", call["call_id"], str(image["index"]), "--output", str(saved))
        self.check("asset get saves it to a new private file (0600)",
                   saved.read_bytes()[:8] == b"\x89PNG\r\n\x1a\n" and saved.stat().st_mode & 0o777 == 0o600)
        link = self.mc(name, "asset", "link", call["call_id"], str(image["index"]))
        first = dev_accounts.http_get_bytes(link["url"])
        second = dev_accounts.http_get_bytes(link["url"])
        self.log("download", first=first[0], second=second[0])
        self.check("asset link: a one-time link downloads without a token once, then answers 404 download_expired",
                   first[0] == 200 and first[1][:8] == b"\x89PNG\r\n\x1a\n" and second[0] == 404
                   and b"download_expired" in second[1] and link["url"].startswith(self.cfg["backend_url"] + "/api/v1/downloads/"),
                   {"first": first[0], "second": [second[0], second[1][:200].decode(errors="replace")]})

    def refresh_rotation(self, name):
        """Force the stored access token close to expiry: the CLI refreshes once, under its lock, and keeps working."""
        path = self.session_file(name)
        def force():
            stored = json.loads(path.read_text())
            stored["expires_at"] = int(time.time()) + 30
            path.write_text(json.dumps(stored))
            path.chmod(0o600)
            return hashlib.sha256(stored["refresh_token"].encode()).hexdigest()[:12]
        before = force()
        self.mc(name, "connection", "ls")
        after = json.loads(path.read_text())
        rotated = hashlib.sha256(after["refresh_token"].encode()).hexdigest()[:12]
        self.check("A token about to expire is refreshed: the refresh token rotated, the file stays owner-only",
                   rotated != before and after["expires_at"] > time.time() + 600 and path.stat().st_mode & 0o777 == 0o600,
                   {"before": before, "after": rotated})
        force()
        processes = [subprocess.Popen([str(self.cli_bin), "--json", "connection", "ls"], env=self.cli_env(self.home(name)),
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for _ in range(3)]
        codes = [process.wait(timeout=120) for process in processes]
        for process in processes:
            process.stdout.close()
            process.stderr.close()
        follow = self.mc(name, "login", "status")
        self.check("Three concurrent commands on an expiring token all succeed (one refresh; no reuse revoked the sign-in)",
                   codes == [0, 0, 0] and follow.get("verified") is True, {"exits": codes, "status": follow})

    def device_denied(self, name):
        process = subprocess.Popen([str(self.cli_bin), "--json", "login", "--label", f"mcport e2e {name}"], env=self.cli_env(self.home(name)),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            first = json.loads(process.stdout.readline())
            status, _ = self.first_party("POST", f"/v1/device/{first['user_code']}/deny", self.c1_first_party())
            out, err = process.communicate(timeout=90)
        except BaseException:
            process.kill()
            raise
        last = json.loads(out.strip().splitlines()[-1])
        self.log("cli", who=name, args=["login"], exit=process.returncode, stdout=out[-1500:], stderr=err[-300:])
        self.check("A denied device code: the CLI stops with device_denied and stays signed out",
                   status == 204 and process.returncode == 1 and last.get("error", {}).get("code") == "device_denied"
                   and self.mc(name, "login", "status") == {"authenticated": False}, last)

    def scenario_2(self):
        s1 = self.mint("silicon", "--custodian-email", self.email["c1"], "--handle", self.handle["s1"])
        self.who["s1"] = {"uuid": s1["uuid"], "id": s1["id"], "stk": s1["stk"]}
        self.check("Silicon created in the Carbon's care", s1["custodian"]["uuid"] == self.who["c1"]["uuid"],
                   {"silicon": s1["id"], "custodian": s1["custodian"]})
        slt = self.mint("slt", "--silicon", s1["id"], "--stk", s1["stk"], "--app", "mcport")["slt"]
        login = self.silicon_login("s1", slt)
        self.check("mcport login --slt-stdin in a fresh home signs the Silicon in",
                   login["authenticated"] is True and login["kind"] == "silicon" and login["uuid"] == s1["uuid"]
                   and login["custodian"]["uuid"] == self.who["c1"]["uuid"], login)
        session_files = list((self.home("s1") / ".mcport").rglob("*.json"))
        self.check("The token never reaches a file; the sign-in file is owner-only",
                   session_files and all("slt_" not in p.read_text() and p.stat().st_mode & 0o077 == 0 for p in session_files),
                   [str(p) for p in session_files])
        status = self.mc("s1", "login", "status")
        self.check("login status --json: authenticated, confirmed by the service",
                   status["authenticated"] is True and status.get("verified") is True and status["uuid"] == s1["uuid"], status)
        created = self.mc("s1", "connection", "new", "s1-notes", "--transport", "http", "--url",
                          self.cfg["providers_url"] + "/mcp/public", "--auth", "none")
        self.who["s1"]["connection"] = created["id"]
        self.check("connection new: the Silicon owns s1-notes", created["access"] == "owner" and created["owner"]["uuid"] == s1["uuid"], created)
        listed = self.mc("s1", "connection", "ls")
        self.check("connection ls lists it", any(c["id"] == created["id"] for c in listed), listed)
        tools = self.mc("s1", "tool", "ls", "s1-notes")
        names = [tool["name"] for tool in tools["result"]["tools"]]
        self.check("tool ls discovers the provider's tools", {"echo", "write", "whoami"} <= set(names), names)
        call = self.mc("s1", "tool", "call", "s1-notes", "echo", "--input", '{"message":"hello from a Silicon"}')
        self.who["s1"]["call"] = call["call_id"]
        self.check("tool call runs as the Silicon", call["result"]["structuredContent"]["arguments"]["message"] == "hello from a Silicon", call)
        activity = self.mc("s1", "activity", "ls")
        mine = [item for item in activity if item["id"] == call["call_id"]]
        self.check("activity ls shows the call with the Silicon as caller", mine and mine[0]["caller"]["uuid"] == s1["uuid"], activity[:3])
        shown = self.mc("s1", "activity", "show", call["call_id"])
        self.check("activity show returns the stored result", shown["result"] == call["result"], shown)
        changed = self.mc("s1", "connection", "set", "s1-notes", "--description", "Notes kept by a Silicon")
        self.check("connection set changes its description", changed["description"] == "Notes kept by a Silicon", changed)
        self.assets_and_tickets("s1", "s1-notes")
        self.refresh_rotation("s1")
        unknown = self.mc("s1", "access", "new", "s1-notes", "--account", f"c:mcport-e2e-nobody-{self.n}", ok=False)
        self.check("Sharing with an id nobody has: 404 unknown_account naming the id",
                   unknown["error"]["code"] == "unknown_account" and f"nobody-{self.n}" in unknown["error"]["message"], unknown)
        out = self.mc("s1", "logout")
        self.check("logout revokes the refresh token and forgets the sign-in", out["signed_out"] is True and out["revoked"] is True, out)
        status = self.mc("s1", "login", "status")
        self.check("login status --json after logout: {\"authenticated\": false}", status == {"authenticated": False}, status)

    # -- scenario 3: device flow ---------------------------------------------------------------------------------

    def device_login(self, name, email):
        home = self.home(name)
        process = subprocess.Popen([str(self.cli_bin), "--json", "login", "--label", f"mcport e2e {name}"], env=self.cli_env(home),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            first = json.loads(process.stdout.readline())
            if first.get("event") != "device_code":
                raise Check(f"mcport login did not start a device sign-in: {first}")
            self.log("cli", who=name, args=["login"], event=first.get("event"), verification_uri=first.get("verification_uri"))
            approved = self.mint("approve", "--email", email, "--code", first["user_code"])
            out, err = process.communicate(timeout=90)
        except BaseException:
            process.kill()
            raise
        lines = [json.loads(line) for line in out.splitlines() if line.strip()]
        self.log("cli", who=name, args=["login"], exit=process.returncode, stdout=out[-2000:], stderr=err[-400:])
        return first, approved, lines[-1] if lines else {}, process.returncode

    def scenario_3(self):
        first, approved, result, code = self.device_login("c1", self.email["c1"])
        self.check("mcport login prints a code and the approval page at Silicon Accounts",
                   first["verification_uri"].startswith(self.cfg["accounts_url"]) and "-" in first["user_code"], first)
        self.check("The Carbon approved the code (204) and the CLI finished signed in",
                   approved["status"] == 204 and code == 0 and result.get("authenticated") is True
                   and result.get("method") == "device" and result.get("uuid") == self.who["c1"]["uuid"], result)
        status = self.mc("c1", "login", "status")
        self.check("login status --json confirms the Carbon", status.get("verified") is True and status["kind"] == "carbon", status)
        listed = self.mc("c1", "connection", "ls")
        mine = [c for c in listed if c["id"] == self.who["s1"]["connection"]]
        self.check("A command as the Carbon: connection ls shows its Silicon's connection (custodian access)",
                   mine and mine[0]["access"] == "custodian" and mine[0]["owner"]["uuid"] == self.who["s1"]["uuid"], listed)
        self.check("The owner is shown with the Silicon's display name and photo, not only its id",
                   mine[0]["owner"]["display_name"] == self.handle["s1"] and (mine[0]["owner"].get("pfp_url") or "").startswith("http"), mine[0]["owner"])
        self.device_denied("c1-denied")

    # -- scenario 4: custodian, circle, sharing ------------------------------------------------------------------

    def c1_first_party(self):
        """A first-party (silicon-accounts) session for the Carbon: creating Silicons, changing their ids."""
        if "first_party" not in self.who["c1"]:
            self.who["c1"]["first_party"] = self.mint("carbon", "--email", self.email["c1"])["access_token"]
        return self.who["c1"]["first_party"]

    def silicon_cli_login(self, name, si_id, stk):
        """The documented Silicon sign-in: silicon-accounts login --app mcport -q | mcport login --slt-stdin."""
        self.sa(name, "login", "--silicon", si_id, "--stk-stdin", "-q", stdin=stk)
        slt = self.sa(name, "login", "--app", "mcport", "-q").stdout.strip()
        if not slt.startswith("slt_"):
            raise Check(f"silicon-accounts login --app mcport -q printed no short-lived token for {si_id}")
        return self.mc(name, "login", "--slt-stdin", stdin=slt)

    def visible(self, name, connection):
        listed = self.mc(name, "connection", "ls")
        return next((c for c in listed if c["id"] == connection), None)

    def scenario_4(self):
        s1, c1 = self.who["s1"], self.who["c1"]
        conn = s1["connection"]
        login = self.silicon_cli_login("s1", s1["id"], s1["stk"])
        self.check("The Silicon signs in again with silicon-accounts login --app mcport -q | mcport login --slt-stdin",
                   login["authenticated"] is True and login["uuid"] == s1["uuid"], login)
        # The custodian sees and manages its Silicon's connection, never acting as it.
        shown = self.mc("c1", "connection", "show", conn)
        self.check("Custodian sees the Silicon's connection and may manage it", shown["can_manage"] is True and shown["access"] == "custodian", shown)
        changed = self.mc("c1", "connection", "set", conn, "--visibility", "circle")
        self.check("Custodian changes its visibility to the Silicon's people (circle)", changed["visibility"] == "circle", changed)
        self.mc("c1", "tool", "set", conn, "echo", "--enabled", "false")
        refused = self.mc("s1", "tool", "call", conn, "echo", "--input", '{"message":"switched off"}', ok=False)
        self.check("Custodian switched the tool off: the Silicon's call is refused (tool_disabled)",
                   refused["error"]["code"] == "tool_disabled", refused)
        self.mc("c1", "tool", "set", conn, "echo", "--enabled", "true")
        activity = self.mc("c1", "activity", "ls")
        theirs = [item for item in activity if item["id"] == s1["call"]]
        self.check("Custodian sees the Silicon's activity", theirs and theirs[0]["caller"]["uuid"] == s1["uuid"], activity[:3])
        result = self.mc("c1", "activity", "show", s1["call"])
        self.check("…and the result of the Silicon's call", result["result"]["structuredContent"]["arguments"]["message"] == "hello from a Silicon", result)
        own = self.mc("c1", "tool", "call", conn, "whoami")
        detail = self.mc("c1", "activity", "show", own["call_id"])
        self.check("A custodian's own call on the Silicon's connection is recorded as the custodian's",
                   detail["caller"]["uuid"] == c1["uuid"], detail)

        # The custodian's other Silicon is in the same circle.
        status, made = self.first_party("POST", "/v1/me/silicons", self.c1_first_party(),
                                        {"id": "si:" + self.handle["s2"], "display_name": "e2e second Silicon"}, expect=(200, 201))
        self.who["s2"] = {"uuid": made["silicon"]["uuid"], "id": made["silicon"]["id"], "stk": made["stk"]}
        self.silicon_cli_login("s2", self.who["s2"]["id"], made["stk"])
        seen = self.visible("s2", conn)
        self.check("The custodian's other Silicon sees the circle connection (access circle)", seen and seen["access"] == "circle", seen)
        used = self.mc("s2", "tool", "call", conn, "echo", "--input", '{"message":"from the other Silicon"}')
        self.check("…and can use it", used["result"]["isError"] is False, used)

        # An unrelated Carbon sees nothing until it is shared with by id.
        first, _, result, code = self.device_login("c2", self.email["c2"])
        self.check("A second, unrelated Carbon signs in (device flow)", code == 0 and result.get("authenticated") is True, result)
        self.who["c2"] = {"uuid": result["uuid"], "id": result["id"]}
        self.check("The unrelated Carbon does not see the Silicon's connection", self.visible("c2", conn) is None)
        hidden = self.mc("c2", "tool", "ls", conn, ok=False)
        self.check("…and gets 404 not_found when it names it", hidden["error"]["code"] == "not_found", hidden)
        granted = self.mc("s1", "access", "new", conn, "--account", self.who["c2"]["id"])
        self.check("The owner shares it with the Carbon by c: id (stored by uuid)", granted["account"]["uuid"] == self.who["c2"]["uuid"], granted)
        seen = self.visible("c2", conn)
        call = self.mc("c2", "tool", "call", conn, "echo", "--input", '{"message":"shared with me"}')
        self.check("The invited Carbon sees (access invited) and uses it",
                   seen and seen["access"] == "invited" and call["result"]["structuredContent"]["arguments"]["message"] == "shared with me", seen)
        hidden_calls = [item for item in self.mc("s1", "activity", "ls") if item["id"] == call["call_id"]]
        self.check("The owner does not see the invited Carbon's calls", hidden_calls == [])
        self.mc("s1", "access", "rm", conn, "--account", self.who["c2"]["id"])
        again = self.mc("c2", "tool", "ls", conn, ok=False)
        self.check("Unsharing removes access at once (404)", again["error"]["code"] == "not_found" and self.visible("c2", conn) is None, again)
        old_result = self.mc("c2", "activity", "show", call["call_id"], ok=False)
        self.check("…and hides the Carbon's earlier results on it", "result" not in old_result and "error" in old_result, old_result)

        # Silicons are not open to the world.
        self.mc("c2", "connection", "new", "c2-tools", "--transport", "http", "--url", self.cfg["providers_url"] + "/mcp/public", "--auth", "none")
        refused = self.mc("c2", "access", "new", "c2-tools", "--account", s1["id"], ok=False)
        self.check("Sharing with a Silicon outside one's people is refused: silicon_not_reachable",
                   refused["error"]["code"] == "silicon_not_reachable" and "allow add" in (refused["error"].get("recovery") or ""), refused)
        allowed = self.mc("c1", "allow", "add", self.who["c2"]["id"], "--silicon", s1["id"])
        self.check("The custodian allows that Carbon for its Silicon", allowed["account"]["uuid"] == self.who["c2"]["uuid"], allowed)
        listed = self.mc("s1", "allow", "ls")
        self.check("The Silicon's allow list shows it", any(a["account"]["uuid"] == self.who["c2"]["uuid"] for a in listed), listed)
        self.mc("c2", "access", "new", "c2-tools", "--account", s1["id"])
        seen = next((c for c in self.mc("s1", "connection", "ls") if c["name"] == "c2-tools"), None)
        self.check("Now the share reaches the Silicon (access invited)", seen and seen["access"] == "invited", seen)
        removed = self.mc("s1", "allow", "rm", self.who["c2"]["id"])
        self.check("The Silicon itself removes the allowance", removed["deleted"] is True, removed)
        self.mc("c2", "connection", "new", "c2-more", "--transport", "http", "--url", self.cfg["providers_url"] + "/mcp/public", "--auth", "none")
        refused = self.mc("c2", "access", "new", "c2-more", "--account", s1["id"], ok=False)
        still = next((c for c in self.mc("s1", "connection", "ls") if c["name"] == "c2-tools"), None)
        self.check("New shares are refused again; what was already shared stays",
                   refused["error"]["code"] == "silicon_not_reachable" and still is not None, refused)
        carbon = self.mc("c2", "access", "new", "c2-more", "--account", c1["id"])
        self.check("Carbons can be reached by anyone signed in (no allowance needed)", carbon["account"]["uuid"] == c1["uuid"], carbon)
        circle_only = self.mc("c1", "connection", "new", "c1-team", "--transport", "http", "--url",
                              self.cfg["providers_url"] + "/mcp/public", "--auth", "none", "--visibility", "circle")
        s1_sees = self.visible("s1", circle_only["id"])
        self.check("A Carbon's circle connection reaches the Silicons it looks after, not other Carbons",
                   s1_sees and s1_sees["access"] == "circle" and self.visible("c2", circle_only["id"]) is None, s1_sees)
        self.who["c1"]["circle_connection"] = circle_only["id"]
        self.scenario_4_more()

    def scenario_4_more(self):
        s1, s2, c1, c2 = self.who["s1"], self.who["s2"], self.who["c1"], self.who["c2"]
        conn = s1["connection"]
        by_uuid = self.mc("s1", "access", "new", conn, "--account", c2["uuid"])
        self.check("Sharing by uuid works too and shows the current id", by_uuid["account"]["id"] == c2["id"], by_uuid)
        self.mc("s1", "access", "rm", conn, "--account", c2["uuid"])
        s2_notes = self.mc("s2", "connection", "new", "s2-notes", "--transport", "http", "--url",
                           self.cfg["providers_url"] + "/mcp/public", "--auth", "none")
        s2["connection"] = s2_notes["id"]

        # Provider accounts: a per-user connection; the custodian may inspect and disconnect, never connect, for its Silicon.
        self.mc("c1", "connection", "new", "c1-bearer", "--transport", "http", "--url", self.cfg["providers_url"] + "/mcp/bearer",
                "--auth", "per-user", "--visibility", "circle")
        missing = self.mc("s1", "tool", "call", "c1-bearer", "whoami", ok=False)
        self.check("Per-user connection without the Silicon's own provider account: refused, no fallback",
                   missing["error"]["code"] == "provider_authentication_required", missing)
        self.mc("s1", "account", "connect", "c1-bearer", "--input", '{"kind":"bearer","secret":"fixture-silicon-token","label":"Silicon fixture"}')
        whoami = self.mc("s1", "tool", "call", "c1-bearer", "whoami")
        self.check("The Silicon connects its own provider account and its calls run as it",
                   whoami["result"]["structuredContent"]["account"] == "provider-silicon", whoami)
        seen = self.mc("c1", "account", "show", "c1-bearer", "--account", s1["id"])
        self.check("The custodian inspects the Silicon's provider account (no secret shown)",
                   seen["connected"] is True and "secret" not in json.dumps(seen) and "fixture-silicon-token" not in json.dumps(seen), seen)
        self.mc("c1", "account", "disconnect", "c1-bearer", "--account", s1["id"])
        again = self.mc("s1", "tool", "call", "c1-bearer", "whoami", ok=False)
        self.check("The custodian disconnects it; the Silicon's next call is refused",
                   again["error"]["code"] == "provider_authentication_required", again)
        outsider = self.mc("c2", "account", "show", "c1-bearer", "--account", s1["id"], ok=False)
        self.check("An unrelated Carbon cannot inspect it", outsider["error"]["code"] in ("not_found", "forbidden", "not_custodian"), outsider)

        # Directory entries: personal to their creator and its custodian, shared by id.
        entry_file = self.dir / "directory-entry.json"
        entry_file.write_text(json.dumps({"name": f"e2e-notes-{self.n}", "description": "A Silicon's saved setup", "category": "Testing",
                                          "template": {"transport": "http", "url": self.cfg["providers_url"] + "/mcp/public",
                                                       "command": None, "args": [], "auth_mode": "none"}}))
        entry = self.mc("s1", "directory", "new", "--input", "@" + str(entry_file))
        custodian_view = self.mc("c1", "directory", "show", entry["id"])
        hidden = self.mc("c2", "directory", "show", entry["id"], ok=False)
        self.check("A Silicon's directory entry: personal, managed by its custodian, hidden from others",
                   entry["source"] == "personal" and custodian_view["can_manage"] is True and hidden["error"]["code"] == "not_found", custodian_view)
        self.mc("s1", "directory", "share", entry["id"], "--account", c2["id"])
        shared = self.mc("c2", "directory", "show", entry["id"])
        made = self.mc("c2", "connection", "new", "from-directory", "--from", entry["id"])
        self.check("Shared by id, the Carbon reads it (no management) and makes its own connection from it",
                   shared["can_manage"] is False and made["owner"]["uuid"] == c2["uuid"], made)
        self.mc("s1", "directory", "unshare", entry["id"], "--account", c2["id"])
        self.check("Unshared: hidden again", self.mc("c2", "directory", "show", entry["id"], ok=False)["error"]["code"] == "not_found")

        # A local stdio MCP on the Carbon's machine, served by its host daemon, used by its Silicon.
        host = self.mc("c1", "host", "new", f"e2e-host-{self.n}")
        self.who["c1"]["host"] = host["host"]["id"]
        local = self.mc("c1", "connection", "new", "c1-local", "--host", host["host"]["id"], "--transport", "stdio",
                        "--command", sys.executable, "--arg", str(ROOT / "tests/e2e/fixtures.py"), "--arg=--stdio",
                        "--env", "FIXTURE_ACCOUNT=stdio-e2e", "--auth", "none", "--visibility", "circle")
        ready = self.wait("the local connection to become ready",
                          lambda: (lambda c: c if c["status"] == "ready" else None)(self.mc("c1", "connection", "show", local["id"])), timeout=40)
        through = self.mc("s1", "tool", "call", local["id"], "whoami")
        self.check("The Silicon calls a stdio MCP on its custodian's machine through the host daemon",
                   ready["status"] == "ready" and through["result"]["structuredContent"]["account"] == "stdio-e2e", through)
        status = self.mc("c1", "daemon", "status")
        self.check("The daemon keys its registry by account uuid (registry version 2)", "registry_version" in json.dumps(status) and '"registry_version":2' in json.dumps(status, separators=(",", ":")), status)
        self.mc("c1", "daemon", "stop")
        self.wait("the local connection to go offline", lambda: self.mc("c1", "connection", "show", local["id"])["status"] != "ready", timeout=60)
        offline = self.mc("s1", "tool", "call", local["id"], "whoami", ok=False)
        self.check("With the daemon stopped the local connection is offline and never rerouted (host_offline)",
                   offline["error"]["code"] == "host_offline", offline)
        self.mc("c1", "daemon", "start")
        self.wait("the local connection to come back", lambda: self.mc("c1", "connection", "show", local["id"])["status"] == "ready", timeout=60)
        self.check("After daemon start the Silicon's calls work again",
                   self.mc("s1", "tool", "call", local["id"], "whoami")["result"]["structuredContent"]["account"] == "stdio-e2e")
        removed = self.mc("c1", "host", "rm", host["host"]["id"])
        self.check("host rm deletes the host and stops its daemon", removed.get("deleted") is True, removed)

        # Settings are per account; a bug report with no mail service configured is not a silent success.
        setting = self.mc("c1", "config", "set", "telemetry", "false")
        mine = self.api("GET", "/api/v1/settings", self.session_access_token("c1"), expect=200)[1]["data"]
        theirs = self.api("GET", "/api/v1/settings", self.session_access_token("s1"), expect=200)[1]["data"]
        self.check("Telemetry switched off for the Carbon only (settings are per account)",
                   setting["server_updated"] is True and mine["telemetry"] is False and theirs["telemetry"] is True, [mine, theirs])
        report = self.mc("c1", "report", f"End-to-end report {self.n}: nothing is wrong; this checks the report path.", ok=False)
        self.check("A report without a configured mail service is recorded and says delivery_failed",
                   report.get("status") == "delivery_failed" and bool(report.get("id")), report)

    # -- scenario 5: webhooks ------------------------------------------------------------------------------------

    def owner_id(self, name, connection):
        shown = self.mc(name, "connection", "show", connection)
        return shown["owner"]["id"]

    def change_silicon_id(self, new_id):
        self.first_party("POST", f"/v1/me/silicons/{self.who['s1']['uuid']}/id", self.c1_first_party(), {"id": new_id}, expect=200)
        return self.wait(f"the service shows {new_id}", lambda: self.owner_id("c1", self.who["s1"]["connection"]) == new_id)

    def id_change_delivery(self, new_id):
        _, page = self.accounts_app("GET", "/v1/apps/mcport/webhook/deliveries?limit=100", expect=200)
        for item in page["items"]:
            if item["type"] == "account.id_changed" and item.get("account_uuid") == self.who["s1"]["uuid"]:
                _, detail = self.accounts_app("GET", f"/v1/apps/mcport/webhook/deliveries/{item['id']}", expect=200)
                if detail["payload"]["data"]["new_id"] == new_id:
                    return detail
        return None

    def session_access_token(self, name):
        return json.loads(self.session_file(name).read_text())["access_token"]

    def scenario_5(self):
        s1, conn = self.who["s1"], self.who["s1"]["connection"]
        first_id, second_id = f"si:mcport-e2e-s1x-{self.n}", f"si:mcport-e2e-s1y-{self.n}"
        self.change_silicon_id(first_id)
        self.check("The custodian changed the Silicon's id; account.id_changed reached MCPort and the new id shows",
                   self.owner_id("s2", conn) == first_id)
        status = self.mc("s1", "login", "status")
        self.check("The Silicon's own login status shows its new id (the uuid stays)", status["id"] == first_id and status["uuid"] == s1["uuid"], status)
        self.change_silicon_id(second_id)
        first = self.wait("the first id change's delivery", lambda: self.id_change_delivery(first_id))
        self.check("Silicon Accounts delivered the first id change", first["status"] == "delivered", {k: first.get(k) for k in ("id", "status", "event_id")})
        _, replay = self.accounts_app("POST", "/v1/apps/mcport/webhook/replay", {"delivery_ids": [first["id"]]}, expect=200)
        self.check("Silicon Accounts replays the first event (same event_id)", first["id"] in replay["replayed"], replay)

        def replayed():
            _, detail = self.accounts_app("GET", f"/v1/apps/mcport/webhook/deliveries/{first['id']}", expect=200)
            # attempt_count restarts with each replay; the attempts list keeps every attempt.
            done = (detail.get("manual_replays", 0) >= 1 and detail["status"] == "delivered"
                    and len(detail.get("attempts") or []) > len(first.get("attempts") or []))
            return detail if done else None
        detail = self.wait("the replayed delivery's outcome", replayed)
        self.check("The replayed event_id was acknowledged and ignored: the newest id stays",
                   detail["attempts"][-1]["status_code"] == 200 and self.owner_id("c1", conn) == second_id,
                   {"attempts": detail["attempts"][-2:]})

        event = {"event_id": f"mcport-e2e-ping-{self.n}", "type": "ping", "occurred_at": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()),
                 "app_id": "mcport", "silicon": None, "data": {}}
        self.who["ping"] = event
        raw, headers = self.sign_webhook(event)
        first_answer, second_answer = self.deliver(raw, headers), self.deliver(raw, headers)
        self.check("A signed delivery is accepted once; the same event_id again answers duplicate",
                   first_answer == (200, {"received": True}) and second_answer == (200, {"received": True, "duplicate": True}),
                   [first_answer, second_answer])
        forged_raw, forged_headers = self.sign_webhook(dict(event, event_id=f"mcport-e2e-forged-{self.n}"), secret="whsec_not-the-real-secret-000000")
        forged = self.deliver(forged_raw, forged_headers)
        self.check("A delivery signed with another secret is refused (401 invalid_webhook_signature)",
                   forged[0] == 401 and forged[1]["error"]["code"] == "invalid_webhook_signature", forged)
        stale_raw, stale_headers = self.sign_webhook(dict(event, event_id=f"mcport-e2e-stale-{self.n}"), timestamp=int(time.time()) - 900)
        stale = self.deliver(stale_raw, stale_headers)
        tampered = self.deliver(raw.replace(b'"data":{}', b'"data":{"x":1}'), dict(headers, **{"X-Accounts-Event-Id": "tampered"}))
        self.check("A captured delivery replayed 15 minutes later, or with a changed body, is refused (401)",
                   stale[0] == 401 and tampered[0] == 401, [stale, tampered])

        old_token = self.session_access_token("s1")
        s1["removed_token"] = old_token
        self.check("The Silicon's current access token works before it removes MCPort",
                   self.api("GET", "/api/v1/connections", old_token)[0] == 200)
        removed = self.sa("s1", "--json", "apps", "remove", "mcport")
        self.check("The Silicon removes MCPort at Silicon Accounts (DELETE /v1/me/apps/mcport)", removed.returncode == 0, removed.stdout[-300:])
        refused = self.wait("the old token to be refused",
                            lambda: (lambda r: r if r[0] == 401 else None)(self.api("GET", "/api/v1/connections", old_token)))
        self.check("membership.access_removed reached MCPort: the old access token is refused (401 signed_out)",
                   refused[1]["error"]["code"] == "signed_out", refused[1])
        live = self.api("POST", f"/api/v1/connections/{conn}/mcp", old_token, {"method": "tools/list"})
        self.check("…on live routes too", live[0] == 401, live[1])
        ended = self.mc("s1", "connection", "ls", ok=False)
        self.check("The CLI says the sign-in ended (refresh refused) and how to sign in again",
                   ended["error"]["code"] == "sign_in_ended", ended)
        self.check("While its access is removed the custodian still sees the Silicon's connection; its other Silicon does not",
                   self.visible("c1", conn) is not None and self.visible("s2", conn) is None)
        back = self.silicon_cli_login("s1", second_id, s1["stk"])
        self.check("Signing in again restores the Silicon's access and its connections",
                   back["authenticated"] is True and self.visible("s1", conn) is not None and self.visible("s2", conn) is not None, back)
        self.scenario_5_more()

    def c2_first_party(self):
        if "first_party" not in self.who["c2"]:
            self.who["c2"]["first_party"] = self.mint("carbon", "--email", self.email["c2"])["access_token"]
        return self.who["c2"]["first_party"]

    def scenario_5_more(self):
        s2, c1, c2 = self.who["s2"], self.who["c1"], self.who["c2"]
        s2_conn = s2["connection"]
        # mcport logout revokes one sign-in (app_revoked, which MCPort ignores); live routes see it at once.
        token = self.session_access_token("s2")
        self.mc("s2", "logout")
        read = self.api("GET", "/api/v1/connections", token)
        live = self.api("POST", f"/api/v1/connections/{s2_conn}/mcp", token, {"method": "tools/list"})
        self.check("After mcport logout the old access token still reads until it expires, but live routes refuse it (sign_in_revoked)",
                   read[0] == 200 and live[0] == 401 and live[1]["error"]["code"] == "sign_in_revoked", {"read": read[0], "live": live[1]})
        self.silicon_cli_login("s2", s2["id"], s2["stk"])

        # The custodian renames the Silicon: account.updated carries the new display name.
        self.first_party("PATCH", f"/v1/me/silicons/{s2['uuid']}", self.c1_first_party(), {"display_name": "Renamed by its custodian"}, expect=200)
        renamed = self.wait("the new display name to show",
                            lambda: (lambda c: c if c and c["owner"]["display_name"] == "Renamed by its custodian" else None)(self.visible("c1", s2_conn)))
        self.check("account.updated: the custodian's rename shows on the Silicon's connection", renamed is not None)

        # The custodian rotates the Silicon's STK: every sign-in ends (membership.signed_out, stk_rotated).
        token = self.session_access_token("s2")
        _, rotated = self.first_party("POST", f"/v1/me/silicons/{s2['uuid']}/stk", self.c1_first_party(), {}, expect=200)
        refused = self.wait("the old token to be refused after the STK rotation",
                            lambda: (lambda r: r if r[0] == 401 else None)(self.api("GET", "/api/v1/connections", token)))
        self.check("STK rotated: membership.signed_out (stk_rotated) refuses the Silicon's older tokens", refused[1]["error"]["code"] == "signed_out", refused[1])
        self.check("The CLI reports the ended sign-in", self.mc("s2", "connection", "ls", ok=False)["error"]["code"] == "sign_in_ended")
        s2["stk"] = rotated["stk"]
        self.sa("s2", "logout", "-q", ok=False)
        self.silicon_cli_login("s2", s2["id"], s2["stk"])
        self.check("With the new STK the Silicon signs in again", self.visible("s2", s2_conn) is not None)

        # The Silicon moves to another custodian: silicon.custodian_changed moves custodian powers and its circle.
        _, transfer = self.first_party("POST", f"/v1/me/silicons/{s2['uuid']}/transfer", self.c1_first_party(), {"to": c2["id"]}, expect=(200, 201))
        _, pending = self.first_party("GET", "/v1/me/custodian-requests", self.c2_first_party(), expect=200)
        request = next((r for r in pending["items"] if r["id"] == transfer["request"]["id"]), None)
        self.check("The transfer is waiting for the second Carbon", request is not None, pending)
        self.first_party("POST", f"/v1/me/custodian-requests/{request['id']}/accept", self.c2_first_party(), expect=(200, 204))
        moved = self.wait("the new custodian to see the Silicon's connection",
                          lambda: (lambda c: c if c and c["access"] == "custodian" else None)(self.visible("c2", s2_conn)))
        self.check("Custodian changed: the new custodian manages the Silicon's connection", moved["can_manage"] is True, moved)
        self.check("…and the previous custodian lost it at once", self.visible("c1", s2_conn) is None)
        self.check("The Silicon left the old custodian's circle: it no longer sees that Carbon's circle connection",
                   self.visible("s2", c1["circle_connection"]) is None and self.visible("s2", self.who["s1"]["connection"]) is None)

        # The new custodian deletes the Silicon: account.deleted removes what it owned.
        token = self.session_access_token("s2")
        self.first_party("DELETE", f"/v1/me/silicons/{s2['uuid']}", self.c2_first_party(), {"confirm": s2["id"]}, expect=(200, 204))
        deleted = self.wait("the deleted Silicon's token to be refused",
                            lambda: (lambda r: r if r[0] == 401 else None)(self.api("GET", "/api/v1/connections", token)))
        self.check("account.deleted: the Silicon's tokens are refused (account_deleted)", deleted[1]["error"]["code"] == "account_deleted", deleted[1])
        gone = self.mc("c2", "connection", "show", s2_conn, ok=False)
        self.check("…and its connections are deleted", gone["error"]["code"] == "not_found", gone)
        self.who["s2"]["deleted"] = True

        # The second Carbon deletes its account: what it owned goes, what others own only loses its access.
        before = {c["name"] for c in self.mc("s1", "connection", "ls")}
        self.first_party("DELETE", "/v1/me", self.c2_first_party(), {"confirm": c2["id"]}, expect=(200, 204))
        self.wait("the deleted Carbon's connections to disappear",
                  lambda: "c2-tools" not in {c["name"] for c in self.mc("s1", "connection", "ls")})
        self.check("account.deleted for a Carbon: the connections it shared are gone for the Silicon it shared with",
                   "c2-tools" in before and self.visible("c1", self.who["c1"]["circle_connection"]) is not None)
        c1_view = {c["name"] for c in self.mc("c1", "connection", "ls")}
        self.check("…and for the Carbon it shared one with; that Carbon's own connections stay", "c2-more" not in c1_view and "c1-team" in c1_view, sorted(c1_view))
        self.who["c2"]["deleted"] = True

        # A copied refresh token used elsewhere: the CLI's next refresh is a reuse, Silicon Accounts ends the sign-in
        # (membership.signed_out, refresh_token_reuse) and MCPort refuses every older token, the thief's included.
        path = self.session_file("s1")
        stored = json.loads(path.read_text())
        status, stolen = http("POST", self.cfg["accounts_api_url"] + "/v1/oauth/token",
                              body={"grant_type": "refresh_token", "refresh_token": stored["refresh_token"], "client_id": "mcport"})
        self.check("A copy of the Silicon's refresh token is redeemed elsewhere (public client)", status == 200 and "access_token" in stolen,
                   {"status": status, "error": (stolen or {}).get("error")})
        stored["expires_at"] = int(time.time()) + 30
        path.write_text(json.dumps(stored))
        path.chmod(0o600)
        ended = self.mc("s1", "connection", "ls", ok=False)
        self.check("The CLI's own refresh is now a reuse: the sign-in ended, and the CLI says so",
                   ended["error"]["code"] == "sign_in_ended" and "already used" in ended["error"]["message"], ended)
        thief = self.wait("the stolen access token to be refused",
                          lambda: (lambda r: r if r[0] == 401 else None)(self.api("GET", "/api/v1/connections", stolen["access_token"])))
        status, again = http("POST", self.cfg["accounts_api_url"] + "/v1/oauth/token",
                             body={"grant_type": "refresh_token", "refresh_token": stolen["refresh_token"], "client_id": "mcport"})
        self.check("refresh_token_reuse reached MCPort: the stolen access token is refused and its refresh token is dead",
                   thief[1]["error"]["code"] == "signed_out" and status == 400 and again.get("error") == "invalid_grant", thief[1])
        self.silicon_cli_login("s1", self.owner_id("c1", self.who["s1"]["connection"]), self.who["s1"]["stk"])

    # -- scenario 6: proofs from other apps ----------------------------------------------------------------------

    def scenario_6(self):
        made_up = self.api("GET", "/api/v1/connections", "sap_not-a-real-proof", scheme="Proof")
        self.check("Authorization: Proof … is refused with proof_not_accepted", made_up[0] == 401 and made_up[1]["error"]["code"] == "proof_not_accepted", made_up[1])
        interface = self.stack.get("apps", {}).get("interface", {}).get("app_secret")
        if not interface:
            self.log("skip", label="real User verification proof: the stack file has no 'interface' app")
            return
        tokens = self.mint("app-signin", "--app", "interface", "--email", self.email["c1"], "--redirect",
                           "http://127.0.0.1:9593/interface/callback", "--exchange")["tokens"]
        self.revoke_later.append(("interface", interface, tokens["refresh_token"]))
        status, proof = self.accounts_app("POST", "/v1/proofs/user-verification", {
            "subject_token": tokens["access_token"], "receiving_app": "mcport", "scopes": ["mcport.connections.read"]},
            app="interface", expect=(200, 201))
        self.check("Silicon Accounts issued a real User verification proof from 'interface' to 'mcport'",
                   proof["proof_token"].startswith("sap_") and proof["receiving_app"] == "mcport", {k: proof.get(k) for k in ("kind", "issuing_app", "receiving_app", "scopes")})
        real = self.api("GET", "/api/v1/connections", proof["proof_token"], scheme="Proof")
        self.check("MCPort honours no proof scopes yet: the valid proof is refused (401 proof_not_accepted)",
                   real[0] == 401 and real[1]["error"]["code"] == "proof_not_accepted", real[1])
        self.accounts_app("POST", "/v1/proofs/revoke", {"proof_id": proof["proof_id"]}, app="interface", expect=(200, 204))

    # -- scenario 7: discovery commands from a packed archive ----------------------------------------------------

    def scenario_7(self):
        version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        machine = (platform.system(), platform.machine())
        target = {("Darwin", "arm64"): "macos-aarch64", ("Darwin", "x86_64"): "macos-x86_64"}.get(machine)
        if target is None:
            self.log("skip", label=f"packing on {machine}: development builds here are not static; CI packs the release builds")
            return
        out = self.dir / "package"
        result = subprocess.run(["bash", str(ROOT / "scripts/package-apps.sh"), version, target, str(self.cli_bin), "--output-dir", str(out)],
                                capture_output=True, text=True, timeout=600)
        self.log("package", target=target, exit=result.returncode, stdout=result.stdout[-2500:], stderr=result.stderr[-1500:])
        archive = out / f"mcport-{version}-{target}.tar.gz"
        self.check(f"scripts/package-apps.sh packed and validated the {target} archive", result.returncode == 0 and archive.is_file(),
                   result.stderr[-800:])
        extracted = self.dir / "extracted"
        extracted.mkdir()
        with tarfile.open(archive) as bundle:
            names = sorted(bundle.getnames())
            bundle.extractall(extracted, filter="data")
        manifest = (extracted / "apps.yaml").read_text()
        self.check("The archive holds apps.yaml (this target only) and bin/mcport",
                   [name for name in names if name != "bin"] == ["apps.yaml", "bin/mcport"] and f"app_id: mcport" in manifest and f"version: {version}" in manifest
                   and target in manifest and manifest.count("binary:") == 1, {"names": names})
        empty = self.dir / "empty-home"
        empty.mkdir()
        env = {"PATH": "/usr/bin:/bin", "HOME": str(empty), "SILICON_HOME": str(empty), "TMPDIR": str(empty), "LANG": "C", "APPS_TELEMETRY": "0"}
        outcomes = {}
        for args in (["--help"], ["accounts", "--json"], ["login", "status", "--json"]):
            run = subprocess.run([str(extracted / "bin" / "mcport"), *args], capture_output=True, text=True, env=env, cwd=empty, timeout=30)
            outcomes[" ".join(args)] = (run.returncode, run.stdout)
            self.log("discovery", args=args, exit=run.returncode, stdout=run.stdout[-1200:], stderr=run.stderr[-300:])
        accounts = json.loads(outcomes["accounts --json"][1])
        self.check("From the extracted archive, in an empty home: --help, accounts --json and login status --json exit 0",
                   all(code == 0 for code, _ in outcomes.values()) and len(outcomes["--help"][1]) > 200, {k: v[0] for k, v in outcomes.items()})
        self.check("accounts --json names app_id mcport and the version; login status --json says signed out",
                   accounts["app_id"] == "mcport" and accounts["version"] == version
                   and json.loads(outcomes["login status --json"][1]) == {"authenticated": False}, accounts)
        self.check("The discovery commands wrote nothing into the empty home", list(empty.iterdir()) == [], [p.name for p in empty.iterdir()])

    # -- scenario 8: restart safety ------------------------------------------------------------------------------

    def scenario_8(self):
        restarted = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "restart"], capture_output=True, text=True, timeout=60)
        self.log("restart", exit=restarted.returncode, stdout=restarted.stdout, stderr=restarted.stderr[-600:])
        self.check("The service restarted with the same data and webhook secret", restarted.returncode == 0 and json.loads(restarted.stdout)["restarted"] is True,
                   restarted.stderr[-400:])
        listed = self.mc("c1", "connection", "ls")
        self.check("The Carbon's stored sign-in still works after the restart (stateless tokens)", any(c["id"] == self.who["s1"]["connection"] for c in listed))
        call = self.mc("s1", "tool", "call", self.who["s1"]["connection"], "echo", "--input", '{"message":"after a restart"}')
        self.check("The Silicon's stored sign-in still works after the restart", call["result"]["isError"] is False, call)
        me = self.api("GET", "/api/v1/me", self.who["c1"]["api_token"])
        self.check("The API token from scenario 1 is still accepted", me[0] == 200 and me[1]["data"]["uuid"] == self.who["c1"]["uuid"], me[1])
        refused = self.api("GET", "/api/v1/connections", self.who["s1"]["removed_token"])
        self.check("Revocations persisted: the Silicon's token from before it removed MCPort is still refused",
                   refused[0] == 401 and refused[1]["error"]["code"] == "signed_out", refused[1])
        raw, headers = self.sign_webhook(self.who["ping"])
        again = self.deliver(raw, headers)
        self.check("Webhook dedupe survived the restart: the earlier event_id still answers duplicate",
                   again == (200, {"received": True, "duplicate": True}), again)
        outcome, event = dev_accounts.test_delivery(self.cfg)
        self.check("Silicon Accounts still delivers to the restarted service (test ping delivered)", outcome == "delivered", outcome)

    # -- run -----------------------------------------------------------------------------------------------------

    def stack_up(self):
        status = dev_accounts.summary(self.cfg)
        if status["pids"]["service"] and status["pids"]["providers"]:
            return
        result = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "up"], capture_output=True, text=True, timeout=180)
        self.log("dev-accounts up", exit=result.returncode, stdout=result.stdout, stderr=result.stderr[-800:])
        if result.returncode != 0:
            raise SystemExit(f"e2e-accounts: the development stack did not start:\n{result.stderr}")
        self.started_stack = True

    def cleanup(self):
        for name in list(self.homes):
            if name.startswith("accounts-"):
                continue
            try:
                if (self.home(name) / ".mcport").exists():
                    self.mc(name, "daemon", "stop", ok=None)  # a host daemon this run may have left running
                self.mc(name, "logout", ok=None)
            except Exception as error:  # cleanup must reach every sign-in
                print(f"  cleanup: logout in {name} failed: {error}", flush=True)
        for name in ("s1", "s2"):
            if ("accounts-" + name) in self.homes:
                self.sa(name, "logout", "-q", ok=False)
        for app, secret, refresh in self.revoke_later:
            http("POST", self.cfg["accounts_api_url"] + "/v1/oauth/revoke", body={"token": refresh, "token_type_hint": "refresh_token"}, basic=(app, secret))
        if self.started_stack and not self.args.keep_running:
            subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "down"], capture_output=True, timeout=60)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", help="comma-separated scenario numbers (they build on each other: 1-3 set up 4-8)")
    parser.add_argument("--keep-running", action="store_true", help="leave the development stack running afterwards")
    args = parser.parse_args()
    run = Run(args)
    chosen = [int(x) for x in args.only.split(",")] if args.only else list(range(1, 9))
    print(f"run {run.n}: {run.dir}", flush=True)
    failed = None
    try:
        run.stack_up()
        for number in chosen:
            print(f"== scenario {number}", flush=True)
            getattr(run, f"scenario_{number}")()
    except (Check, dev_accounts.Failure, subprocess.TimeoutExpired, KeyError, TypeError, ValueError) as error:
        failed = f"{type(error).__name__}: {error}"
        print(f"FAIL {failed}", flush=True)
    finally:
        run.cleanup()
        identities = {name: {k: v for k, v in who.items() if k in ("uuid", "id", "connection")} for name, who in run.who.items() if name != "ping"}
        result = {"run": run.n, "passed": len(run.checks), "failed": failed, "scenarios": chosen, "identities": identities,
                  "checks": run.checks, "run_dir": str(run.dir)}
        (run.dir / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({k: result[k] for k in ("run", "passed", "failed", "run_dir")}))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
