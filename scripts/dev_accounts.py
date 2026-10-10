#!/usr/bin/env python3
"""Run MCPort on this machine against a local Silicon Accounts stack.

    scripts/dev-accounts.sh [--build]        # dev_accounts.py up: start (idempotent)
    scripts/dev-accounts-stop.sh             # dev_accounts.py down: stop what up started
    python3 scripts/dev_accounts.py restart  # restart the service only (same data, same webhook secret)
    python3 scripts/dev_accounts.py status   # what runs, as JSON (never secrets)

`up` starts, when not already running:
  - the MCP fixture providers (tests/e2e/fixtures.py) on 127.0.0.1:<base+2>;
  - mcport-server on 127.0.0.1:<base+1>, data in <dir>/server, trusting the stack's access tokens;
and points MCPort's webhook at Silicon Accounts to http://127.0.0.1:<base+1>/webhooks/accounts with MCPort's own
app credentials (PUT /v1/apps/mcport/webhook). The webhook's signing secret is kept in <dir>/webhook-secret (mode
0600, never in git) and reaches the service only through its environment. A test ping then proves that deliveries
arrive and verify; a stale secret is replaced and the service restarted.

Configuration (environment):
  MCPORT_TEST_STACK   JSON file describing the stack: accounts_public_url, accounts_api_url and
                      apps.mcport.app_secret (the shape of the Silicon Accounts testkit's stack file)
  ACCOUNTS_URL        Silicon Accounts public URL, the token issuer (default: stack file, else http://localhost:9590)
  ACCOUNTS_API_URL    where MCPort calls Silicon Accounts (default: stack file, else ACCOUNTS_URL)
  MCPORT_APP_SECRET   mcport's app secret at that stack (default: stack file)
  MCPORT_DEV_BASE     port block base (default 4240: website 4240, service 4241, providers 4242)
  MCPORT_DEV_DIR      state directory (default .local/dev-accounts): server data, logs, pids, webhook secret
  MCPORT_DEV_PIDS     pid directory (default <dir>/pids)
  MCPORT_SERVER_BIN   mcport-server binary (default $CARGO_TARGET_DIR/debug/mcport-server, else target/debug)

Only loopback Silicon Accounts URLs are accepted: this script never talks to a deployed Silicon Accounts.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlparse
from urllib.request import ProxyHandler, Request, build_opener
import uuid

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "mcport"
WEBHOOK_PATH = "/webhooks/accounts"
LOOPBACK = {"localhost", "127.0.0.1", "::1"}
# Never forward live mail or telemetry credentials into a development service.
SCRUBBED = ("POSTMARK_SERVER_TOKEN", "SPACE_STATION_API_KEY", "SPACESTATION_API_KEY", "MCPORT_TELEMETRY_KEY",
            "MCPORT_TELEMETRY_URL", "MCPORT_MASTER_KEY")
OPENER = build_opener(ProxyHandler({}))


class Failure(Exception):
    """A precise, user-facing reason the command cannot continue."""


def http(method, url, body=None, basic=None, bearer=None, headers=None, timeout=15):
    """One JSON request; returns (status, parsed body or None). Network errors raise Failure."""
    options = {"Accept": "application/json", **(headers or {})}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        options["Content-Type"] = "application/json"
    if basic:
        options["Authorization"] = "Basic " + base64.b64encode(":".join(basic).encode()).decode()
    if bearer:
        options["Authorization"] = "Bearer " + bearer
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=timeout) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        with error:
            status, raw = error.code, error.read()
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url} failed: {getattr(error, 'reason', error)}") from None
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, {"raw": raw[:300].decode(errors="replace")}


def http_raw(method, url, data, headers):
    """Send exact body bytes (webhook deliveries are signed over them); returns (status, parsed body or None)."""
    options = {"Content-Type": "application/json", **headers}
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=15) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        with error:
            status, raw = error.code, error.read()
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url} failed: {getattr(error, 'reason', error)}") from None
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, {"raw": raw[:300].decode(errors="replace")}


def http_get_bytes(url):
    """GET without credentials; returns (status, body bytes)."""
    try:
        with OPENER.open(Request(url), timeout=15) as response:
            return response.status, response.read()
    except HTTPError as error:
        with error:
            return error.code, error.read()
    except (URLError, OSError) as error:
        raise Failure(f"GET {url} failed: {getattr(error, 'reason', error)}") from None


def loopback_url(name, value):
    parsed = urlparse(value)
    if parsed.scheme not in ("http", "https") or (parsed.hostname or "") not in LOOPBACK:
        raise Failure(f"{name} must be a local Silicon Accounts stack on this machine (localhost or 127.0.0.1); got {value!r}. "
                      "This script never talks to a deployed Silicon Accounts.")
    return value.rstrip("/")


def load_config():
    env = os.environ.get
    stack = {}
    stack_file = env("MCPORT_TEST_STACK")
    if stack_file:
        try:
            stack = json.loads(Path(stack_file).read_text())
        except (OSError, ValueError) as error:
            raise Failure(f"MCPORT_TEST_STACK={stack_file} is not a readable JSON stack file: {error}") from None
    accounts_url = loopback_url("ACCOUNTS_URL", env("ACCOUNTS_URL") or stack.get("accounts_public_url") or "http://localhost:9590")
    api_url = loopback_url("ACCOUNTS_API_URL", env("ACCOUNTS_API_URL") or stack.get("accounts_api_url") or accounts_url)
    secret = env("MCPORT_APP_SECRET") or stack.get("apps", {}).get(APP_ID, {}).get("app_secret")
    if not secret:
        raise Failure("mcport's app secret at the stack is missing: set MCPORT_APP_SECRET, or MCPORT_TEST_STACK to a stack "
                      "file with apps.mcport.app_secret.")
    try:
        base = int(env("MCPORT_DEV_BASE", "4240"))
    except ValueError:
        raise Failure("MCPORT_DEV_BASE must be a port number (the website's port; the service uses base+1).") from None
    dev_dir = Path(env("MCPORT_DEV_DIR") or ROOT / ".local" / "dev-accounts").resolve()
    target = Path(env("CARGO_TARGET_DIR") or ROOT / "target")
    if not target.is_absolute():
        target = ROOT / target
    return {
        "accounts_url": accounts_url,
        "accounts_api_url": api_url,
        "app_secret": secret,
        "base": base,
        "service_port": base + 1,
        "providers_port": base + 2,
        "backend_url": f"http://127.0.0.1:{base + 1}",
        "providers_url": f"http://127.0.0.1:{base + 2}",
        "web_url": f"http://localhost:{base}",
        "webhook_url": f"http://127.0.0.1:{base + 1}{WEBHOOK_PATH}",
        "dir": dev_dir,
        "pids": Path(env("MCPORT_DEV_PIDS") or dev_dir / "pids").resolve(),
        "logs": dev_dir / "logs",
        "data": dev_dir / "server",
        "secret_file": dev_dir / "webhook-secret",
        "server_bin": Path(env("MCPORT_SERVER_BIN") or target / "debug" / "mcport-server"),
    }


# -- processes ---------------------------------------------------------------------------------------------------

EXPECTED = {"service": "mcport-server", "providers": "fixtures.py"}


def command_of(pid):
    result = subprocess.run(["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else ""


def running(cfg, name):
    """The pid recorded for `name` when that process still runs the expected program, else None."""
    path = cfg["pids"] / name
    try:
        pid = int(path.read_text().strip())
    except (OSError, ValueError):
        return None
    if EXPECTED[name] not in command_of(pid):
        path.unlink(missing_ok=True)  # stale: never signal a process this script did not start
        return None
    return pid


def port_open(port):
    with socket.socket() as probe:
        probe.settimeout(0.3)
        return probe.connect_ex(("127.0.0.1", port)) == 0


def listener_pid(port):
    result = subprocess.run(["lsof", "-nP", "-t", f"-iTCP:{port}", "-sTCP:LISTEN"], capture_output=True, text=True)
    pids = [int(line) for line in result.stdout.split() if line.isdigit()]
    return pids[0] if pids else None


def tail(path, lines=15):
    try:
        return "\n".join(path.read_text(errors="replace").splitlines()[-lines:])
    except OSError:
        return "(no log)"


def spawn(cfg, name, command, env, port, ready_path):
    if port_open(port):
        raise Failure(f"127.0.0.1:{port} is already in use by another program (pid {listener_pid(port)}); "
                      f"stop it or choose another MCPORT_DEV_BASE.")
    cfg["logs"].mkdir(parents=True, exist_ok=True)
    cfg["pids"].mkdir(parents=True, exist_ok=True)
    log = cfg["logs"] / f"{name}.log"
    with log.open("a") as handle:
        handle.write(f"\n--- {time.strftime('%Y-%m-%dT%H:%M:%S')} starting {name}\n")
        handle.flush()
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=handle, stderr=subprocess.STDOUT,
                                   stdin=subprocess.DEVNULL, start_new_session=True)
    url = f"http://127.0.0.1:{port}{ready_path}"
    for _ in range(150):
        if process.poll() is not None:
            raise Failure(f"{name} exited with status {process.returncode} before it answered {url}:\n{tail(log)}")
        try:
            status, _ = http("GET", url, timeout=1)
            if status == 200:
                # Interpreter shims can fork: record the process that actually listens.
                pid = listener_pid(port) or process.pid
                (cfg["pids"] / name).write_text(f"{pid}\n")
                return pid
        except Failure:
            pass
        time.sleep(0.1)
    process.terminate()
    raise Failure(f"{name} did not answer {url} within 15 seconds:\n{tail(log)}")


def stop(cfg, name):
    pid = running(cfg, name)
    if pid is None:
        return False
    os.kill(pid, signal.SIGTERM)
    for _ in range(100):
        if not command_of(pid):
            break
        time.sleep(0.1)
    else:
        os.kill(pid, signal.SIGKILL)
    (cfg["pids"] / name).unlink(missing_ok=True)
    return True


# -- Silicon Accounts --------------------------------------------------------------------------------------------

def app_call(cfg, method, path, body=None, expected=(200,)):
    status, value = http(method, cfg["accounts_api_url"] + path, body=body, basic=(APP_ID, cfg["app_secret"]),
                         headers={"Idempotency-Key": str(uuid.uuid4())} if method != "GET" else None)
    if status not in expected:
        error = (value or {}).get("error", value)
        raise Failure(f"Silicon Accounts refused {method} {path} with HTTP {status}: {json.dumps(error)}")
    return value


def read_secret(cfg):
    try:
        secret = cfg["secret_file"].read_text().strip()
    except OSError:
        return None
    return secret if secret.startswith("whsec_") else None


def write_secret(cfg, secret):
    cfg["dir"].mkdir(parents=True, exist_ok=True)
    cfg["dir"].chmod(0o700)
    path = cfg["secret_file"]
    temporary = path.with_suffix(".tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w") as handle:
        handle.write(secret + "\n")
    os.replace(temporary, path)


def new_secret(cfg):
    """Make Silicon Accounts sign with a fresh secret (it works before a URL is set) and keep it here."""
    secret = app_call(cfg, "POST", f"/v1/apps/{APP_ID}/webhook/generate-secret")["secret"]
    write_secret(cfg, secret)
    return secret


def ensure_webhook(cfg):
    """Point MCPort's webhook at this service. PUT keeps the stored signing secret, so the local copy stays valid."""
    secret = read_secret(cfg) or new_secret(cfg)
    current = app_call(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
    if (current or {}).get("url") != cfg["webhook_url"] or not current.get("secret_set"):
        saved = app_call(cfg, "PUT", f"/v1/apps/{APP_ID}/webhook", {"url": cfg["webhook_url"]})
        if saved.get("secret"):  # Accounts had no secret and made one: that one signs from now on.
            secret = saved["secret"]
            write_secret(cfg, secret)
    return secret


def test_delivery(cfg, timeout=25):
    """Queue a ping and wait for its outcome: 'delivered', or the receiver's last HTTP status as text."""
    queued = app_call(cfg, "POST", f"/v1/apps/{APP_ID}/webhook/test", expected=(200, 202))
    delivery = queued["delivery_id"]
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        detail = app_call(cfg, "GET", f"/v1/apps/{APP_ID}/webhook/deliveries/{delivery}")
        if detail.get("status") == "delivered":
            return "delivered", queued["event_id"]
        attempts = detail.get("attempts") or []
        if attempts:
            last = attempts[-1]
            if last.get("status_code") in (400, 401, 403, 503):
                return f"HTTP {last['status_code']}: {last.get('error')}", queued["event_id"]
        time.sleep(0.5)
    return f"no successful delivery within {timeout} s (last attempt: {json.dumps(last)})", queued["event_id"]


# -- commands ----------------------------------------------------------------------------------------------------

def service_env(cfg, secret):
    env = {key: value for key, value in os.environ.items() if key not in SCRUBBED and not key.startswith("MCPORT_")}
    env.update(
        MCPORT_BIND=f"127.0.0.1:{cfg['service_port']}",
        MCPORT_PUBLIC_URL=cfg["backend_url"],
        MCPORT_WEB_URL=cfg["web_url"],
        MCPORT_APP_ID=APP_ID,
        MCPORT_APP_SECRET=cfg["app_secret"],
        MCPORT_ACCOUNTS_WEBHOOK_SECRET=secret,
        MCPORT_DATA_DIR=str(cfg["data"]),
        MCPORT_ALLOWED_UPSTREAM_ORIGINS=cfg["providers_url"],
        ACCOUNTS_URL=cfg["accounts_url"],
        ACCOUNTS_API_URL=cfg["accounts_api_url"],
        RUST_LOG=os.environ.get("RUST_LOG", "mcport_server=info"),
        NO_COLOR="1",
    )
    return env


def start_providers(cfg):
    pid = running(cfg, "providers")
    if pid:
        return pid, False
    env = {key: value for key, value in os.environ.items() if key not in SCRUBBED}
    command = [sys.executable, "-I", str(ROOT / "tests/e2e/fixtures.py"), "--port", str(cfg["providers_port"]),
               "--state", str(cfg["dir"] / "providers.json")]
    return spawn(cfg, "providers", command, env, cfg["providers_port"], "/health"), True


def start_service(cfg, secret):
    pid = running(cfg, "service")
    if pid:
        return pid, False
    if not cfg["server_bin"].is_file():
        raise Failure(f"{cfg['server_bin']} does not exist: build it (cargo build -p mcport-server), pass --build, "
                      "or set MCPORT_SERVER_BIN.")
    cfg["data"].mkdir(parents=True, exist_ok=True)
    cfg["data"].chmod(0o700)
    return spawn(cfg, "service", [str(cfg["server_bin"])], service_env(cfg, secret), cfg["service_port"], "/health"), True


def build():
    subprocess.run(["cargo", "build", "--locked", "-p", "mcport-server", "-p", "mcport-cli"], cwd=ROOT, check=True)


def up(cfg, args):
    if args.build:
        build()
    status, _ = http("GET", cfg["accounts_api_url"] + "/.well-known/jwks.json", timeout=5)
    if status != 200:
        raise Failure(f"Silicon Accounts at {cfg['accounts_api_url']} did not serve its JWKS (HTTP {status}); start the stack first.")
    cfg["dir"].mkdir(parents=True, exist_ok=True)
    cfg["dir"].chmod(0o700)
    providers, providers_started = start_providers(cfg)
    secret = ensure_webhook(cfg)
    service, service_started = start_service(cfg, secret)
    outcome, event = test_delivery(cfg)
    if outcome != "delivered":
        if not service_started:
            raise Failure(f"The running service did not accept a test delivery ({outcome}). It may hold an older "
                          "webhook secret: run dev_accounts.py restart.")
        print(f"dev-accounts: the test delivery failed ({outcome}); making a new webhook secret.", file=sys.stderr)
        secret = new_secret(cfg)
        stop(cfg, "service")
        service, service_started = start_service(cfg, secret)
        outcome, event = test_delivery(cfg)
        if outcome != "delivered":
            raise Failure(f"Silicon Accounts still cannot deliver to {cfg['webhook_url']}: {outcome}")
    print(json.dumps(summary(cfg) | {"started": {"providers": providers_started, "service": service_started},
                                     "webhook_test": {"event_id": event, "outcome": outcome}}, indent=2))


def down(cfg, _args):
    stopped = {name: stop(cfg, name) for name in ("service", "providers")}
    print(json.dumps({"stopped": stopped}))


def restart(cfg, _args):
    secret = read_secret(cfg)
    if not secret:
        raise Failure(f"No webhook secret in {cfg['secret_file']}: run dev-accounts.sh (up) first.")
    stopped = stop(cfg, "service")
    pid, _ = start_service(cfg, secret)
    print(json.dumps({"restarted": stopped, "service": pid}))


def summary(cfg):
    return {
        "accounts_url": cfg["accounts_url"],
        "accounts_api_url": cfg["accounts_api_url"],
        "backend_url": cfg["backend_url"],
        "providers_url": cfg["providers_url"],
        "website_url": cfg["web_url"],
        "webhook_url": cfg["webhook_url"],
        "data_dir": str(cfg["data"]),
        "logs": str(cfg["logs"]),
        "pids": {name: running(cfg, name) for name in ("service", "providers")},
    }


def status(cfg, _args):
    print(json.dumps(summary(cfg), indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("up", "down", "restart", "status"))
    parser.add_argument("--build", action="store_true", help="with up: cargo build the service and CLI first")
    args = parser.parse_args()
    try:
        cfg = load_config()
        {"up": up, "down": down, "restart": restart, "status": status}[args.command](cfg, args)
    except Failure as failure:
        print(f"dev-accounts: {failure}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
