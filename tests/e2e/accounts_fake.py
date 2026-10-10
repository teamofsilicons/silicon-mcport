#!/usr/bin/env python3
"""A loopback-only fake Silicon Accounts for the end-to-end journey; never part of the app.

It speaks the parts of the Silicon Accounts API that MCPort uses, with the same shapes and rules (docs:
silicon-accounts/docs/reference/api): Ed25519-signed access tokens and their JWKS, the device flow and short-lived
token exchange for MCPort's public client, rotating refresh tokens (presenting a used one revokes the sign-in),
revocation, introspection, account lookups (public identity only), MCPort's user base (names and photos), the
app webhook (configured with PUT /v1/apps/mcport/webhook, deliveries signed `v1=` HMAC-SHA256 over
"{timestamp}.{body}") and its test ping.

Fixture-only endpoints under /fixture/ create Carbons and Silicons, mint short-lived tokens, approve or deny device
codes and make the account changes that send webhook events (id change, rename, STK rotation, removed access,
custodian transfer, deletion). Every credential here is fake and valid only inside this process.

    python3 tests/e2e/accounts_fake.py --port 4253 [--app-secret S] [--state FILE]
"""
import argparse
import base64
import hashlib
import hmac
import json
from pathlib import Path
import secrets
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, unquote, urlparse
from urllib.request import ProxyHandler, Request, build_opener
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ed25519  # noqa: E402

APP_ID = "mcport"
DEFAULT_SECRET = "sa_app_mcport_fixture_secret_not_real"
ALPHABET = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
USER_CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789"
SLT_GRANTS = ("urn:silicon:params:oauth:grant-type:slt", "slt")
DEVICE_GRANTS = ("urn:ietf:params:oauth:grant-type:device_code", "device_code")
OPENER = build_opener(ProxyHandler({}))


def b64url(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def iso(seconds, millis=0):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(seconds)) + f".{millis:03d}Z"


def iso_now():
    now = time.time()
    return iso(int(now), int(now * 1000) % 1000)


class Oauth(Exception):
    """An RFC 6749 error answer (400 unless given)."""

    def __init__(self, error, description, status=400):
        super().__init__(description)
        self.error, self.description, self.status = error, description, status


class ApiError(Exception):
    """A Silicon Accounts API error answer."""

    def __init__(self, status, code, message, hint=""):
        super().__init__(message)
        self.status, self.code, self.message, self.hint = status, code, message, hint


class Fake:
    def __init__(self, origin, app_secret):
        self.origin = origin
        self.app_secret = app_secret
        self.lock = threading.RLock()
        self.seed = secrets.token_bytes(32)
        self.public = ed25519.public_key(self.seed)
        self.kid = "fake-" + b64url(hashlib.sha256(self.public).digest()[:6])
        self.accounts = {}       # uuid -> account
        self.families = {}       # fid -> {uuid, refresh, used, revoked: None | (at, reason)}
        self.by_refresh = {}     # refresh token -> fid (current and used ones)
        self.slts = {}           # slt -> {uuid, expires, used_at}
        self.devices = {}        # device_code -> {user_code, label, status, uuid, expires, exchanged}
        self.webhook = {"url": None, "secret": None}
        self.deliveries = {}     # delivery id -> record
        self.stats = {"introspections": 0, "lookups": 0, "user_reads": 0, "tokens": 0, "refreshes": 0, "revocations": 0}
        self.token_ttl = 1800

    # -- accounts --------------------------------------------------------------------------------------------

    def new_uuid(self):
        while True:
            value = "".join(secrets.choice(ALPHABET) for _ in range(3))
            if value not in self.accounts:
                return value

    def create(self, kind, handle, display_name=None, custodian=None):
        prefix = "c:" if kind == "carbon" else "si:"
        account_id = prefix + handle.split(":", 1)[-1]
        with self.lock:
            if any(a["id"] == account_id for a in self.accounts.values() if a["status"] != "deleted"):
                raise ApiError(409, "id_taken", f"{account_id} is taken.")
            if kind == "silicon" and custodian not in self.accounts:
                raise ApiError(422, "validation_failed", "A Silicon needs its custodian's uuid.")
            account = {"uuid": self.new_uuid(), "kind": kind, "id": account_id,
                       "display_name": display_name or handle.replace("-", " ").title(),
                       "pfp_url": f"{self.origin}/pfp/{kind}.png", "status": "active",
                       "custodian": custodian if kind == "silicon" else None,
                       "membership": None, "version": 1}
            account["pfp_url"] += f"?id={account['uuid']}"
            self.accounts[account["uuid"]] = account
            return dict(account)

    def account(self, account_uuid):
        account = self.accounts.get(account_uuid)
        if account is None:
            raise ApiError(404, "account_not_found", f"No account has the uuid {account_uuid}.")
        return account

    def public_view(self, account):
        """What an app's lookup answers: the public identity only."""
        view = {"uuid": account["uuid"], "kind": account["kind"], "id": account["id"], "status": account["status"]}
        if account["kind"] == "silicon":
            custodian = self.accounts.get(account["custodian"])
            view["custodian"] = {"uuid": custodian["uuid"], "id": custodian["id"]} if custodian else None
        return view

    def app_view(self, account):
        """The account as MCPort may see it after a sign-in (token responses, user base)."""
        view = {"uuid": account["uuid"], "membership_id": f"{APP_ID}:{account['uuid']}", "kind": account["kind"],
                "id": account["id"], "display_name": account["display_name"], "pfp_url": account["pfp_url"],
                "updated_at": iso_now(), "version": account["version"]}
        if account["kind"] == "silicon":
            custodian = self.accounts.get(account["custodian"])
            view["custodian"] = {"uuid": custodian["uuid"], "id": custodian["id"]} if custodian else None
        return view

    # -- tokens ----------------------------------------------------------------------------------------------

    def sign(self, claims):
        header = b64url(json.dumps({"alg": "EdDSA", "typ": "JWT", "kid": self.kid}, separators=(",", ":")).encode())
        payload = b64url(json.dumps(claims, separators=(",", ":")).encode())
        signing_input = f"{header}.{payload}".encode()
        return f"{header}.{payload}.{b64url(ed25519.sign(self.seed, signing_input, self.public))}"

    def claims_of(self, token):
        """The claims of an access token this fake signed (signature checked by recomputing it), else None."""
        parts = token.split(".")
        if len(parts) != 3:
            return None
        try:
            claims = json.loads(base64.urlsafe_b64decode(parts[1] + "=" * (-len(parts[1]) % 4)))
            expected = self.sign_raw(f"{parts[0]}.{parts[1]}".encode())
        except (ValueError, UnicodeDecodeError):
            return None
        return claims if hmac.compare_digest(expected, parts[2]) else None

    def sign_raw(self, signing_input):
        return b64url(ed25519.sign(self.seed, signing_input, self.public))

    def issue(self, account, fid=None, method="slt"):
        """A token response for `account`: a new sign-in (family) unless `fid` names one to rotate."""
        now = int(time.time())
        with self.lock:
            if fid is None:
                fid = str(uuid.uuid4())
                self.families[fid] = {"uuid": account["uuid"], "refresh": None, "revoked": None, "method": method}
                account["membership"] = "active"
            refresh = "sar_" + secrets.token_urlsafe(32)
            family = self.families[fid]
            family["refresh"] = refresh
            self.by_refresh[refresh] = fid
            self.stats["tokens"] += 1
            claims = {"iss": self.origin, "sub": account["uuid"], "aud": APP_ID, "exp": now + self.token_ttl, "iat": now, "nbf": now,
                      "jti": str(uuid.uuid4()), "kind": account["kind"], "id": account["id"], "mid": f"{APP_ID}:{account['uuid']}",
                      "fid": fid, "scope": "profile"}
            return {"access_token": self.sign(claims), "token_type": "Bearer", "expires_in": self.token_ttl,
                    "refresh_token": refresh, "refresh_token_expires_at": iso(now + 900 * 86400), "scope": "profile",
                    "membership_id": f"{APP_ID}:{account['uuid']}", "account": self.app_view(account)}

    def revoke_family(self, fid, reason):
        family = self.families[fid]
        if family["revoked"] is None:
            family["revoked"] = (iso_now(), reason)
            return True
        return False

    def revoke_account(self, account_uuid, reason):
        with self.lock:
            return [fid for fid, family in self.families.items()
                    if family["uuid"] == account_uuid and self.revoke_family(fid, reason)]

    # -- grants ----------------------------------------------------------------------------------------------

    def device_authorize(self, body):
        if body.get("client_id") != APP_ID:
            raise ApiError(400, "invalid_client", f"No app has the client_id {body.get('client_id')!r} here.")
        code = "".join(secrets.choice(USER_CODE_ALPHABET) for _ in range(8))
        user_code = code[:4] + "-" + code[4:]
        device_code = "sad_" + secrets.token_urlsafe(24)
        with self.lock:
            self.devices[device_code] = {"user_code": user_code, "label": body.get("client_label", ""), "status": "pending",
                                         "uuid": None, "expires": time.time() + 600, "exchanged": False}
        return {"device_code": device_code, "user_code": user_code, "verification_uri": f"{self.origin}/device",
                "verification_uri_complete": f"{self.origin}/device?code={user_code}", "expires_in": 600, "interval": 1,
                "expires_at": iso(int(time.time()) + 600)}

    def decide_device(self, user_code, decision, account_uuid=None):
        with self.lock:
            device = next((d for d in self.devices.values() if d["user_code"] == user_code.upper()), None)
            if device is None:
                raise ApiError(404, "device_code_not_found", f"No device sign-in has the code {user_code}.")
            if device["status"] != "pending":
                raise ApiError(409, "device_code_used", "This code was already decided.")
            if decision == "approve":
                account = self.account(account_uuid)
                if account["kind"] != "carbon":
                    raise ApiError(403, "carbon_only", "Only Carbons approve device sign-ins.")
                device["uuid"] = account_uuid
            device["status"] = "approved" if decision == "approve" else "denied"

    def token(self, form):
        grant = form.get("grant_type", "")
        if grant in DEVICE_GRANTS:
            with self.lock:
                device = self.devices.get(form.get("device_code", ""))
                if device is None:
                    raise Oauth("invalid_grant", "This device code is not known.")
                if device["expires"] < time.time():
                    raise Oauth("expired_token", "The device code expired; start a new sign-in.")
                if device["status"] == "pending":
                    raise Oauth("authorization_pending", "The Carbon hasn't approved this device code yet; keep polling.")
                if device["status"] == "denied":
                    raise Oauth("access_denied", "The Carbon denied this sign-in.")
                if device["exchanged"]:
                    raise Oauth("invalid_grant", "This device code was already exchanged.")
                device["exchanged"] = True
                account = self.account(device["uuid"])
            return self.issue(account, method="device")
        if grant in SLT_GRANTS:
            with self.lock:
                slt = self.slts.get(form.get("slt", ""))
                if slt is None:
                    raise Oauth("invalid_grant", "This short-lived token is not known (it may be mistyped or from another Silicon Accounts).")
                if slt["used_at"]:
                    raise Oauth("invalid_grant", f"The short-lived token was already used at {slt['used_at']}.")
                if slt["expires"] < time.time():
                    raise Oauth("invalid_grant", f"The short-lived token expired at {iso(int(slt['expires']))}.")
                slt["used_at"] = iso_now()
                account = self.account(slt["uuid"])
                if account["status"] != "active":
                    raise Oauth("invalid_grant", "The account that minted this short-lived token is not active.")
            return self.issue(account, method="slt")
        if grant == "refresh_token":
            reuse = None
            with self.lock:
                fid = self.by_refresh.get(form.get("refresh_token", ""))
                if fid is None:
                    raise Oauth("invalid_grant", "This refresh token is not known.")
                family = self.families[fid]
                account = self.account(family["uuid"])
                if family["revoked"]:
                    at, reason = family["revoked"]
                    raise Oauth("invalid_grant", f"The sign-in this refresh token belongs to was revoked at {at} ({reason}); sign in again.")
                if family["refresh"] != form["refresh_token"]:
                    self.revoke_family(fid, "refresh_token_reuse")
                    reuse = account
                else:
                    self.stats["refreshes"] += 1
            if reuse:
                self.send("membership.signed_out", reuse, {"reason": "refresh_token_reuse"})
                raise Oauth("invalid_grant", "This refresh token was already used once. Presenting a used refresh token revokes the whole "
                                             "sign-in to protect the account, so this sign-in is now revoked; sign in again.")
            return self.issue(account, fid=fid)
        raise Oauth("unsupported_grant_type", f"This fake supports the device-code, short-lived token and refresh grants, not {grant!r}.")

    def revoke(self, form):
        token = form.get("token", "")
        with self.lock:
            self.stats["revocations"] += 1
            fid = self.by_refresh.get(token)
            if fid is None:
                claims = self.claims_of(token)
                fid = claims.get("fid") if claims and claims.get("fid") in self.families else None
            if fid is None:
                return {"revoked": False, "message": "Nothing was revoked: this is not a token issued to 'mcport'."}
            revoked = self.revoke_family(fid, "app_revoked")
            account = self.account(self.families[fid]["uuid"])
        if revoked:
            self.send("membership.signed_out", account, {"reason": "app_revoked"})
        return {"revoked": True}

    def introspect(self, form):
        with self.lock:
            self.stats["introspections"] += 1
            claims = self.claims_of(form.get("token", ""))
            if not claims or claims.get("aud") != APP_ID or claims["exp"] < time.time():
                return {"active": False}
            family = self.families.get(claims.get("fid"))
            account = self.accounts.get(claims["sub"])
            if family is None or family["revoked"] or account is None or account["status"] != "active":
                return {"active": False}
        return {"active": True, "iss": claims["iss"], "sub": claims["sub"], "aud": APP_ID, "client_id": APP_ID, "exp": claims["exp"],
                "iat": claims["iat"], "nbf": claims["nbf"], "jti": claims["jti"], "kind": claims["kind"], "id": claims["id"],
                "username": claims["id"], "membership_id": claims["mid"], "scope": claims["scope"], "token_type": "access_token"}

    def mint_slt(self, account_uuid):
        with self.lock:
            account = self.account(account_uuid)
            if account["kind"] != "silicon":
                raise ApiError(403, "silicon_only", "Short-lived tokens are for Silicons (Carbons use the device flow here).")
            slt = "slt_" + secrets.token_urlsafe(32)
            self.slts[slt] = {"uuid": account_uuid, "expires": time.time() + 120, "used_at": None}
            return {"slt": slt, "app_id": APP_ID, "expires_at": iso(int(time.time()) + 120)}

    # -- webhook ---------------------------------------------------------------------------------------------

    def new_secret(self):
        with self.lock:
            self.webhook["secret"] = "whsec_" + secrets.token_urlsafe(32)
            return {"secret": self.webhook["secret"]}

    def set_webhook(self, body):
        with self.lock:
            self.webhook["url"] = body["url"]
            made = None
            if not self.webhook["secret"]:
                made = self.new_secret()["secret"]
            return {"url": self.webhook["url"], "secret": made, "events": None}

    def send(self, event_type, account, data, to_any_account=False):
        """Deliver one event to MCPort's webhook now (signed like Silicon Accounts); returns the delivery record."""
        with self.lock:
            url, secret = self.webhook["url"], self.webhook["secret"]
            member = account is None or to_any_account or account.get("membership") == "active"
        if not url or not secret or not member:
            return None
        payload = dict(data)
        if account is not None:
            payload = {"uuid": account["uuid"], "membership_id": f"{APP_ID}:{account['uuid']}", **payload}
        event = {"app_id": APP_ID, "data": payload, "event_id": str(uuid.uuid4()), "occurred_at": iso_now(), "silicon": None, "type": event_type}
        return self.deliver(event, url, secret)

    def deliver(self, event, url, secret):
        raw = json.dumps(event, separators=(",", ":")).encode()
        timestamp = str(int(time.time()))
        signature = hmac.new(secret.encode(), timestamp.encode() + b"." + raw, hashlib.sha256).hexdigest()
        headers = {"Content-Type": "application/json", "User-Agent": "SiliconAccounts-Webhooks/1", "X-Accounts-Event-Id": event["event_id"],
                   "X-Accounts-Event-Type": event["type"], "X-Accounts-Timestamp": timestamp, "X-Accounts-Signature": "v1=" + signature}
        record = {"id": str(uuid.uuid4()), "event_id": event["event_id"], "type": event["type"], "url": url, "payload": event,
                  "account_uuid": event["data"].get("uuid"), "attempts": []}
        try:
            with OPENER.open(Request(url, data=raw, headers=headers, method="POST"), timeout=10) as response:
                record["attempts"].append({"status_code": response.status, "error": None, "body": response.read(500).decode(errors="replace")})
        except HTTPError as error:
            with error:
                record["attempts"].append({"status_code": error.code, "error": f"HTTP {error.code}", "body": error.read(500).decode(errors="replace")})
        except (URLError, OSError) as error:
            record["attempts"].append({"status_code": None, "error": str(getattr(error, "reason", error))})
        code = record["attempts"][-1]["status_code"]
        record["status"] = "delivered" if code and 200 <= code < 300 else "failed"
        record["attempt_count"] = len(record["attempts"])
        with self.lock:
            self.deliveries[record["id"]] = record
        return record

    # -- account changes (fixture) ---------------------------------------------------------------------------

    def change(self, account_uuid, action, body):
        """Apply one account change the way Silicon Accounts does and send MCPort its event."""
        with self.lock:
            account = self.account(account_uuid)
            if account["status"] != "active":
                raise ApiError(409, "account_not_active", f"{account['id']} is not active.")
            if action == "id":
                old = account["id"]
                prefix = "c:" if account["kind"] == "carbon" else "si:"
                account["id"] = prefix + body["id"].split(":", 1)[-1]
                account["version"] += 1
                event = ("account.id_changed", {"kind": account["kind"], "old_id": old, "new_id": account["id"]})
            elif action == "rename":
                account["display_name"] = body["display_name"]
                account["version"] += 1
                event = ("account.updated", {"changed": ["display_name"], "account": self.app_view(account)})
            elif action == "stk":
                if account["kind"] != "silicon":
                    raise ApiError(400, "not_a_silicon", "Only Silicons have an STK.")
                self.revoke_account(account_uuid, "stk_rotated")
                event = ("membership.signed_out", {"reason": "stk_rotated"})
            elif action == "remove-app":
                self.revoke_account(account_uuid, "access_removed")
                event = ("membership.access_removed", {})
            elif action == "transfer":
                target = self.account(body["to"])
                if account["kind"] != "silicon" or target["kind"] != "carbon":
                    raise ApiError(400, "invalid_transfer", "A Silicon moves to a Carbon.")
                before = self.accounts[account["custodian"]]
                account["custodian"] = target["uuid"]
                account["version"] += 1
                event = ("silicon.custodian_changed", {"from": {"uuid": before["uuid"], "id": before["id"]},
                                                       "to": {"uuid": target["uuid"], "id": target["id"]}})
            elif action == "delete":
                looked_after = [a["id"] for a in self.accounts.values() if a["custodian"] == account_uuid and a["status"] == "active"]
                if looked_after:
                    raise ApiError(409, "custodian_of_silicons", f"{account['id']} is the custodian of {', '.join(looked_after)}.")
                self.revoke_account(account_uuid, "account_deleted")
                event = ("account.deleted", {})
            else:
                raise ApiError(404, "route_not_found", f"No account change {action!r}.")
        record = self.send(event[0], account, event[1])
        with self.lock:
            if action == "remove-app":
                account["membership"] = "access_removed"
            if action == "delete":
                account["status"] = "deleted"
        return {"account": self.public_view(account), "delivery": record and {k: record[k] for k in ("id", "event_id", "type", "status")}}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass  # requests carry tokens; never log them

    @property
    def fake(self):
        return self.server.fake

    def reply(self, status, value=None, content_type="application/json"):
        raw = b"" if value is None else (json.dumps(value).encode() if content_type == "application/json" else value.encode())
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(raw)

    def body(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0) or 0))
        if "application/x-www-form-urlencoded" in self.headers.get("Content-Type", ""):
            return {key: values[0] for key, values in parse_qs(raw.decode()).items()}
        return json.loads(raw or b"{}")

    def basic(self):
        value = self.headers.get("Authorization", "")
        if not value.startswith("Basic "):
            return None
        try:
            return tuple(base64.b64decode(value[6:]).decode().split(":", 1))
        except (ValueError, UnicodeDecodeError):
            return ("", "")

    def require_app(self):
        if self.basic() != (APP_ID, self.fake.app_secret):
            raise ApiError(401, "invalid_app_credentials", "This needs MCPort's app credentials (HTTP Basic app_id:app_secret).")

    def client(self, form):
        """Token-endpoint client authentication: HTTP Basic with the secret, or the public client's client_id alone."""
        basic = self.basic()
        if basic is not None:
            if basic != (APP_ID, self.fake.app_secret) or form.get("client_id", APP_ID) != APP_ID:
                raise Oauth("invalid_client", "Client authentication failed.", 401)
            return
        if form.get("client_id") != APP_ID:
            raise Oauth("invalid_client", "Send client_id=mcport (public client) or HTTP Basic app credentials.", 401)
        if "client_secret" in form and form["client_secret"] != self.fake.app_secret:
            raise Oauth("invalid_client", "Client authentication failed.", 401)

    def handle_any(self, method):
        path = unquote(urlparse(self.path).path)
        parts = [p for p in path.split("/") if p]
        try:
            body = self.body() if method in ("POST", "PUT", "DELETE") else {}
            status, value = self.route(method, path, parts, body)
            self.reply(status, value)
        except Oauth as error:
            self.reply(error.status, {"error": error.error, "error_description": error.description})
        except ApiError as error:
            self.reply(error.status, {"error": {"code": error.code, "message": error.message, "hint": error.hint}})
        except (ValueError, KeyError, json.JSONDecodeError) as error:
            self.reply(400, {"error": {"code": "invalid_request", "message": f"Invalid request: {error}", "hint": ""}})

    do_GET = lambda self: self.handle_any("GET")  # noqa: E731
    do_POST = lambda self: self.handle_any("POST")  # noqa: E731
    do_PUT = lambda self: self.handle_any("PUT")  # noqa: E731
    do_DELETE = lambda self: self.handle_any("DELETE")  # noqa: E731

    def route(self, method, path, parts, body):
        fake = self.fake
        if method == "GET" and path == "/health":
            return 200, {"fake_silicon_accounts": True, "ready": True}
        if method == "GET" and path == "/.well-known/jwks.json":
            return 200, {"keys": [{"kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "use": "sig", "kid": fake.kid, "x": b64url(fake.public)}]}
        if method == "POST" and path == "/v1/device/authorize":
            return 200, fake.device_authorize(body)
        if method == "POST" and path == "/v1/oauth/token":
            self.client(body)
            return 200, fake.token(body)
        if method == "POST" and path == "/v1/oauth/revoke":
            self.client(body)
            return 200, fake.revoke(body)
        if method == "POST" and path == "/v1/oauth/introspect":
            self.require_app()
            return 200, fake.introspect(body)
        if parts[:2] == ["v1", "accounts"] and method == "GET":
            self.require_app()
            with fake.lock:
                fake.stats["lookups"] += 1
                if parts[2:3] == ["by-id"]:
                    account = next((a for a in fake.accounts.values() if a["status"] == "active" and a["id"].lower() == parts[3].lower()), None)
                    if account is None:
                        raise ApiError(404, "account_not_found", f"No account has the id {parts[3]}.")
                else:
                    account = fake.account(parts[2])
                if account["status"] == "deleted":
                    raise ApiError(404, "account_deleted", "This account was deleted.")
                return 200, fake.public_view(account)
        if parts[:3] == ["v1", "apps", APP_ID]:
            self.require_app()
            return self.app_route(method, parts[3:], body)
        if parts[:1] == ["fixture"]:
            return self.fixture_route(method, parts[1:], body)
        raise ApiError(404, "route_not_found", f"The fake Silicon Accounts has no {method} {path}.")

    def app_route(self, method, rest, body):
        fake = self.fake
        if method == "GET" and rest[:1] == ["users"] and len(rest) == 2:
            with fake.lock:
                fake.stats["user_reads"] += 1
                account = fake.accounts.get(rest[1])
                if account is None or account["membership"] is None:
                    raise ApiError(404, "user_not_found", "This account never signed in to MCPort.")
                return 200, {**fake.app_view(account), "status": account["membership"], "account_status": account["status"],
                             "source": "signin", "history": []}
        if rest == ["webhook"] and method == "GET":
            return 200, {"url": fake.webhook["url"], "secret_set": bool(fake.webhook["secret"]), "events": None, "status": "active"}
        if rest == ["webhook"] and method == "PUT":
            return 200, fake.set_webhook(body)
        if rest in (["webhook", "generate-secret"], ["webhook", "rotate-secret"]) and method == "POST":
            return 200, fake.new_secret()
        if rest == ["webhook", "test"] and method == "POST":
            record = fake.send("ping", None, {})
            if record is None:
                raise ApiError(409, "webhook_not_set", "Set the webhook URL first.")
            return 202, {"event_id": record["event_id"], "delivery_id": record["id"], "type": "ping"}
        if rest[:2] == ["webhook", "deliveries"] and method == "GET":
            with fake.lock:
                if len(rest) == 3:
                    if rest[2] not in fake.deliveries:
                        raise ApiError(404, "delivery_not_found", "No such delivery.")
                    return 200, fake.deliveries[rest[2]]
                return 200, {"items": list(reversed(list(fake.deliveries.values()))), "next_cursor": None}
        raise ApiError(404, "route_not_found", "No such app route in the fake.")

    def fixture_route(self, method, rest, body):
        fake = self.fake
        if rest == ["stats"] and method == "GET":
            with fake.lock:
                return 200, dict(fake.stats, deliveries=len(fake.deliveries))
        if rest == ["accounts"] and method == "POST":
            return 200, fake.public_view(fake.account(fake.create(body["kind"], body["handle"], body.get("display_name"), body.get("custodian"))["uuid"]))
        if rest == ["slt"] and method == "POST":
            return 200, fake.mint_slt(body["uuid"])
        if rest == ["token"] and method == "POST":
            return 200, fake.issue(fake.account(body["uuid"]), method="fixture")
        if rest[:1] == ["device"] and len(rest) == 3 and method == "POST":
            fake.decide_device(rest[1], rest[2], body.get("uuid"))
            return 200, {"decided": rest[2], "user_code": rest[1]}
        if rest == ["replay-last"] and method == "POST":
            # Like POST /v1/apps/{app}/webhook/replay: the same event (and event_id), signed again.
            with fake.lock:
                record = next((r for r in reversed(list(fake.deliveries.values())) if r["type"] == body["type"]), None)
                url, secret = fake.webhook["url"], fake.webhook["secret"]
            if record is None:
                raise ApiError(404, "delivery_not_found", f"No {body['type']} delivery to replay.")
            return 200, fake.deliver(record["payload"], url, secret)
        if rest[:1] == ["accounts"] and len(rest) == 3 and method == "POST":
            return 200, fake.change(rest[1], rest[2], body)
        if rest[:1] == ["accounts"] and len(rest) == 2 and method == "DELETE":
            return 200, fake.change(rest[1], "delete", body)
        raise ApiError(404, "route_not_found", "No such fixture route.")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=4253)
    parser.add_argument("--app-secret", default=DEFAULT_SECRET)
    parser.add_argument("--state", help="write the fake's URL and app secret (JSON, 0600) here once it listens")
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    origin = f"http://127.0.0.1:{server.server_address[1]}"
    server.fake = Fake(origin, args.app_secret)
    if args.state:
        path = Path(args.state)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"accounts_public_url": origin, "accounts_api_url": origin,
                                    "apps": {APP_ID: {"app_secret": args.app_secret}}}, indent=1))
        path.chmod(0o600)
    print(json.dumps({"origin": origin}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
