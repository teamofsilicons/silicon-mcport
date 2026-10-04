#!/usr/bin/env python3
"""Loopback-only IAM 5.2.1 wire protocol and MCP fixtures; never part of the app.

The application still uses the official IAM SDK and production authorization code.
All credentials in this file are conspicuously fake and valid only in this process.
"""
import argparse
import base64
import hashlib
import html
import json
import os
import secrets
import socket
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse

TEST_ID = "11111111-1111-4111-8111-111111111111"
TEST_KEY = "0123456789abcdefghijklmnopqrstuv"
APP_ID = "mcport"
APP_SECRET = "fixture-app-secret"
TEST_SECRET = "fixture-test-app-secret"
ACTORS = {
    "owner": {"principal_id": "c:owner", "identity_kind": "carbon", "org_id": "tos"},
    "silicon": {"principal_id": "si:researcher", "identity_kind": "silicon", "org_id": "tos"},
    "stranger": {"principal_id": "c:stranger", "identity_kind": "carbon", "org_id": "tos"},
    "crossorg": {"principal_id": "c:outsider", "identity_kind": "carbon", "org_id": "other"},
}


class Fixtures:
    def __init__(self, origin):
        self.origin = origin
        self.lock = threading.RLock()
        self.slts = {}
        self.tokens = {}
        self.idempotency = {}
        self.revoked = set()
        self.oauth_codes = {}
        self.oauth_tokens = {}
        self.calls = []
        self.requests = []
        self.clients = {}

    def mint_slt(self, role, environment="production"):
        if role not in ACTORS or environment not in ("production", TEST_ID):
            raise ValueError("Unknown fixture role or environment")
        slt = "oac_" + secrets.token_urlsafe(32)
        self.slts[slt] = {"actor": ACTORS[role].copy(), "environment": environment, "expires": time.time() + 300}
        return slt

    def pair(self, grant):
        access = "oat_fixture_" + uuid.uuid4().hex
        refresh = "ort_fixture_" + uuid.uuid4().hex
        self.tokens[access] = dict(grant, expires=time.time() + 3600, kind="access")
        self.tokens[refresh] = dict(grant, expires=time.time() + 86400, kind="refresh")
        return {"access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": 3600, "scope": "self.identity.read", "org_id": grant["actor"]["org_id"]}


def tools():
    return [
        {"name": "echo", "description": "Return the message and provider account", "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"], "additionalProperties": False}},
        {"name": "write", "description": "Record one side effect for replay checks", "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}},
        {"name": "whoami", "description": "Show which upstream account executes this call", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "fail", "description": "Return a legitimate MCP tool error", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "slow", "description": "Wait long enough to test cancellation", "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number", "minimum": 0, "maximum": 10}}}},
        {"name": "nested", "description": "Preserve nested JSON input and mixed output", "inputSchema": {"type": "object", "properties": {"options": {"type": "object"}}, "required": ["options"]}},
        {"name": "drop", "description": "Record a side effect, then lose the HTTP response", "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}},
    ]


def mcp_result(message, account, state=None):
    method, params = message.get("method"), message.get("params", {})
    if method == "initialize":
        return {"protocolVersion": params.get("protocolVersion", "2025-11-25"), "capabilities": {"tools": {}, "resources": {}, "prompts": {}, "completions": {}}, "serverInfo": {"name": "mcport-e2e-fixture", "version": "1.0"}}
    if method in ("notifications/initialized", "notifications/cancelled", "notifications/progress"):
        return None
    if method == "ping":
        return {}
    if method == "completion/complete":
        prefix = params.get("argument", {}).get("value", "")
        return {"completion": {"values": [prefix + "-fixture"], "total": 1, "hasMore": False}}
    if method == "tools/list":
        return {"tools": tools()[3:]} if params.get("cursor") == "page-two" else {"tools": tools()[:3], "nextCursor": "page-two"}
    if method == "tools/call":
        name, arguments = params.get("name"), params.get("arguments", {})
        call = {"tool": name, "account": account, "arguments": arguments}
        if state:
            with state.lock:
                state.calls.append(call)
        if name == "fail":
            return {"isError": True, "content": [{"type": "text", "text": "Fixture tool failed deliberately; fix its inputs before another call."}]}
        if name == "slow":
            time.sleep(min(10, max(0, float(arguments.get("seconds", 3)))))
        result = {"account": account, "arguments": arguments, "tool": name}
        content = [{"type": "text", "text": json.dumps(result)}]
        if name == "nested":
            content.append({"type": "resource_link", "name": "fixture-resource", "uri": "fixture://readme", "mimeType": "text/plain"})
            content.append({"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg=="})
        return {"content": content, "structuredContent": result, "isError": False}
    if method == "resources/list":
        return {"resources": [{"uri": "fixture://readme", "name": "Fixture README", "mimeType": "text/plain"}]}
    if method == "resources/templates/list":
        return {"resourceTemplates": [{"uriTemplate": "fixture://items/{id}", "name": "Fixture item"}]}
    if method == "resources/read":
        return {"contents": [{"uri": params["uri"], "mimeType": "text/plain", "text": "Fixture resource, visible as " + account}]}
    if method == "prompts/list":
        return {"prompts": [{"name": "summarize", "description": "Summarize supplied text", "arguments": [{"name": "text", "required": True}]}]}
    if method == "prompts/get":
        return {"description": "Fixture prompt", "messages": [{"role": "user", "content": {"type": "text", "text": "Summarize: " + str(params.get("arguments", {}).get("text", ""))}}]}
    raise ValueError("Method not found: " + str(method))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    @property
    def fixture(self):
        return self.server.fixture

    def log_message(self, *_):
        pass  # Request URLs can contain OAuth codes. Do not log them.

    def send(self, status, value=None, headers=None, content_type="application/json"):
        raw = b"" if value is None else (json.dumps(value).encode() if content_type == "application/json" else value.encode())
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Silicon-IAM-API-Version", "v1")
        self.send_header("Vary", "Silicon-IAM-Supported-API-Versions")
        for key, item in (headers or {}).items():
            self.send_header(key, item)
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def error(self, message, status=400, code="fixture_invalid"):
        self.send(status, {"error": {"code": code, "message": message}})

    def body(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if "application/x-www-form-urlencoded" in self.headers.get("Content-Type", ""):
            return {key: values[0] for key, values in parse_qs(raw.decode()).items()}
        return json.loads(raw or b"{}")

    def iam_environment(self):
        if self.headers.get("Silicon-IAM-Supported-API-Versions") != "v1":
            raise ValueError("Official IAM supported-version header is required")
        try:
            decoded = base64.b64decode(self.headers.get("Authorization", "").removeprefix("Basic ")).decode()
        except (ValueError, UnicodeDecodeError):
            raise ValueError("Application HTTP Basic authentication is required") from None
        if decoded == APP_ID + ":" + APP_SECRET:
            if self.headers.get("X-Testing-Environment-Key") or self.headers.get("X-Testing-Application"):
                raise ValueError("Production app credential cannot authorize fixture testing")
            return "production"
        if decoded == APP_ID + ":" + TEST_SECRET:
            expected_selector = "Basic " + base64.b64encode(decoded.encode()).decode()
            if self.headers.get("X-Testing-Environment-Key") == TEST_KEY or self.headers.get("X-Testing-Application") == expected_selector:
                return TEST_ID
        raise ValueError("Invalid fixture application credential or testing selector")

    def do_DELETE(self):
        if urlparse(self.path).path.startswith("/mcp/"):
            self.send(204)
        else:
            self.error("Route not found", 404)

    def do_GET(self):
        path = urlparse(self.path).path
        query = {key: values[0] for key, values in parse_qs(urlparse(self.path).query).items()}
        if path == "/health":
            return self.send(200, {"fixture": True, "ready": True})
        if path == "/fixture/stats":
            with self.fixture.lock:
                return self.send(200, {"calls": self.fixture.calls, "requests": self.fixture.requests})
        if path == "/api/v1/application/testing-context":
            try:
                environment = self.iam_environment()
                if environment != TEST_ID:
                    raise ValueError("Not testing")
            except ValueError as error:
                return self.error(str(error), 401)
            return self.send(200, {"environment_id": TEST_ID, "application": {"app_id": APP_ID, "base_url": "http://127.0.0.1:4380", "app_scope": {"iam": ["self.identity.read"], "external": []}, "webhook_scope": [], "testing_idle_days": 30}})
        if path in ("/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp/oauth"):
            return self.send(200, {"resource": self.fixture.origin + "/mcp/oauth", "authorization_servers": [self.fixture.origin], "scopes_supported": ["mcp:tools"], "bearer_methods_supported": ["header"]})
        if path in ("/.well-known/oauth-authorization-server", "/.well-known/openid-configuration"):
            return self.send(200, {"issuer": self.fixture.origin, "authorization_endpoint": self.fixture.origin + "/oauth/authorize", "token_endpoint": self.fixture.origin + "/oauth/token", "registration_endpoint": self.fixture.origin + "/oauth/register", "response_types_supported": ["code"], "grant_types_supported": ["authorization_code", "refresh_token"], "code_challenge_methods_supported": ["S256"], "token_endpoint_auth_methods_supported": ["none"]})
        if path == "/oauth/authorize":
            if query.get("code_challenge_method") != "S256" or not query.get("state") or not query.get("code_challenge"):
                return self.error("OAuth fixture requires state and PKCE S256")
            redirect = query.get("redirect_uri", "")
            if urlparse(redirect).hostname not in ("127.0.0.1", "localhost"):
                return self.error("Fixture callback must use loopback")
            code = "code_fixture_" + uuid.uuid4().hex
            with self.fixture.lock:
                self.fixture.oauth_codes[code] = dict(query, expires=time.time() + 120)
            destination = redirect + ("&" if "?" in redirect else "?") + urlencode({"code": code, "state": query["state"]})
            return self.send(302, None, {"Location": destination})
        if path == "/login":
            kind = query.get("identity_kind", "carbon")
            role = "silicon" if kind == "silicon" else "owner"
            link = "/fixture/iam/approve?" + urlencode(dict(query, role=role))
            document = "<!doctype html><title>Fixture IAM consent</title><h1>Local fixture IAM</h1><p>This is test consent using the official IAM wire protocol.</p><a href='" + html.escape(link, quote=True) + "'>Approve fixture login as " + html.escape(ACTORS[role]["principal_id"]) + "</a>"
            return self.send(200, document, content_type="text/html")
        if path == "/fixture/iam/approve":
            redirect = query.get("redirect_uri", "")
            if urlparse(redirect).hostname not in ("127.0.0.1", "localhost"):
                return self.error("Fixture callback must use loopback")
            with self.fixture.lock:
                slt = self.fixture.mint_slt(query.get("role", "owner"))
            parameters = {"slt": slt}
            if query.get("state") and "state" not in parse_qs(urlparse(redirect).query):
                parameters["state"] = query["state"]
            destination = redirect + ("&" if "?" in redirect else "?") + urlencode(parameters)
            return self.send(302, None, {"Location": destination})
        if path.startswith("/mcp/"):
            return self.send(405, None, {"Allow": "POST, DELETE"})
        self.error("Route not found", 404)

    def do_POST(self):
        path = urlparse(self.path).path
        try:
            body = self.body()
        except (ValueError, json.JSONDecodeError):
            return self.error("Invalid request body")
        with self.fixture.lock:
            self.fixture.requests.append({"method": "POST", "path": path})
        if path == "/fixture/slt":
            try:
                with self.fixture.lock:
                    slt = self.fixture.mint_slt(body.get("role", "owner"), body.get("environment", "production"))
                return self.send(200, {"slt": slt, "actor": ACTORS[body.get("role", "owner")]})
            except ValueError as error:
                return self.error(str(error))
        if path == "/fixture/revoke":
            pair = (body["principal_id"], body.get("environment", "production"))
            with self.fixture.lock:
                if body.get("revoked", True):
                    self.fixture.revoked.add(pair)
                else:
                    self.fixture.revoked.discard(pair)
            return self.send(200, {"updated": True})
        if path.startswith("/api/v1/"):
            return self.iam(path, body)
        if path == "/oauth/register":
            client = "client_fixture_" + uuid.uuid4().hex
            self.fixture.clients[client] = body
            return self.send(201, dict(body, client_id=client, client_id_issued_at=int(time.time()), token_endpoint_auth_method="none"))
        if path == "/oauth/token":
            with self.fixture.lock:
                if body.get("grant_type") == "refresh_token":
                    account = self.fixture.oauth_tokens.pop(body.get("refresh_token"), None)
                    if not account:
                        return self.error("Invalid OAuth refresh token", 401)
                else:
                    attempt = self.fixture.oauth_codes.pop(body.get("code"), None)
                    if not attempt or attempt["expires"] < time.time():
                        return self.error("Invalid OAuth code", 401)
                    challenge = base64.urlsafe_b64encode(hashlib.sha256(body.get("code_verifier", "").encode()).digest()).decode().rstrip("=")
                    if challenge != attempt["code_challenge"] or body.get("redirect_uri") != attempt["redirect_uri"] or body.get("client_id") != attempt.get("client_id") or body.get("resource") != attempt.get("resource"):
                        return self.error("PKCE/client/redirect/resource binding mismatch", 401)
                    account = "oauth-owner"
                access, refresh = "provider_fixture_" + uuid.uuid4().hex, "provider_refresh_fixture_" + uuid.uuid4().hex
                self.fixture.oauth_tokens[access] = account
                self.fixture.oauth_tokens[refresh] = account
            return self.send(200, {"access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": 3600, "scope": "mcp:tools"})
        if path.startswith("/mcp/"):
            token = self.headers.get("Authorization", "").removeprefix("Bearer ")
            account = "public" if path == "/mcp/public" else "desktop-owner"
            if path == "/mcp/bearer":
                account = {"fixture-owner-token": "provider-owner", "fixture-silicon-token": "provider-silicon"}.get(token)
                if account is None:
                    return self.send(401, {"error": "invalid_token"}, {"WWW-Authenticate": "Bearer"})
            if path == "/mcp/oauth":
                account = self.fixture.oauth_tokens.get(token)
                if not account:
                    return self.send(401, {"error": "invalid_token"}, {"WWW-Authenticate": 'Bearer resource_metadata="' + self.fixture.origin + '/.well-known/oauth-protected-resource/mcp/oauth"'})
            try:
                result = mcp_result(body, account, self.fixture)
            except (ValueError, KeyError) as error:
                return self.send(200, {"jsonrpc": "2.0", "id": body.get("id"), "error": {"code": -32601, "message": str(error)}})
            if "id" not in body:
                return self.send(202)
            if body.get("method") == "tools/call" and body.get("params", {}).get("name") == "drop":
                self.close_connection = True
                try:
                    self.connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                self.connection.close()
                return
            return self.send(200, {"jsonrpc": "2.0", "id": body["id"], "result": result})
        self.error("Route not found", 404)

    def iam(self, path, body):
        try:
            environment = self.iam_environment()
        except ValueError as error:
            return self.error(str(error), 401, "unauthenticated")
        if path == "/api/v1/app-auth/tokens":
            key = self.headers.get("Idempotency-Key")
            if not key or len(key) < 16 or body.get("app_id") != APP_ID:
                return self.error("Valid idempotency key and app_id required")
            cache_key = (environment, key, json.dumps(body, sort_keys=True))
            with self.fixture.lock:
                if cache_key in self.fixture.idempotency:
                    return self.send(200, self.fixture.idempotency[cache_key])
                if body.get("slt"):
                    grant = self.fixture.slts.pop(body["slt"], None)
                else:
                    grant = self.fixture.tokens.pop(body.get("refresh_token"), None)
                if not grant or grant["environment"] != environment or grant["expires"] < time.time():
                    return self.error("Invalid, consumed or wrong-environment fixture credential", 401, "invalid_grant")
                result = self.fixture.pair(grant)
                self.fixture.idempotency[cache_key] = result
                return self.send(200, result)
        if path == "/api/v1/oauth/introspect":
            with self.fixture.lock:
                grant = self.fixture.tokens.get(body.get("token"))
                if not grant or grant.get("kind") != "access" or grant["environment"] != environment or grant["expires"] < time.time():
                    return self.send(200, {"active": False})
                actor = grant["actor"]
                if (actor["principal_id"], environment) in self.fixture.revoked or self.headers.get("X-Org-ID", actor["org_id"]) != actor["org_id"]:
                    return self.send(200, {"active": False})
            authorization = {"actor_type": actor["identity_kind"], "public_id": actor["principal_id"], "organization_id": "22222222-2222-4222-8222-222222222222", "org_id": actor["org_id"], "membership_id": actor["principal_id"] + "[" + actor["org_id"] + "]", "membership_version": 1, "authorization_epoch": 1, "audience": APP_ID, "testing_environment_id": None if environment == "production" else environment, "scopes": ["self.identity.read"], "org_role": "member", "tags": []}
            return self.send(200, {"active": True, "public_id": actor["principal_id"], "actor_type": actor["identity_kind"], "client_id": APP_ID, "audience": APP_ID, "org_id": actor["org_id"], "membership_id": authorization["membership_id"], "expires_at": int(grant["expires"]), "authorization": authorization})
        if path == "/api/v1/oauth/revoke":
            with self.fixture.lock:
                self.fixture.tokens.pop(body.get("token"), None)
            return self.send(204)
        self.error("Fixture IAM route not found", 404)


def stdio():
    for line in sys.stdin:
        try:
            message = json.loads(line)
            result = mcp_result(message, os.environ.get("FIXTURE_ACCOUNT", "stdio-owner"))
            if "id" in message:
                print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
        except (ValueError, KeyError) as error:
            print(json.dumps({"jsonrpc": "2.0", "id": message.get("id"), "error": {"code": -32601, "message": str(error)}}), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=4390)
    parser.add_argument("--stdio", action="store_true")
    parser.add_argument("--state", help="Write fixture-only endpoint and sample-SLT metadata")
    args = parser.parse_args()
    if args.stdio:
        return stdio()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    origin = "http://127.0.0.1:" + str(server.server_address[1])
    server.fixture = Fixtures(origin)
    samples = {role: server.fixture.mint_slt(role) for role in ACTORS}
    metadata = {"origin": origin, "iam_url": origin, "public_mcp": origin + "/mcp/public", "bearer_mcp": origin + "/mcp/bearer", "oauth_mcp": origin + "/mcp/oauth", "local_mcp": origin + "/mcp/local", "test_id": TEST_ID, "test_key": TEST_KEY, "app_secret": APP_SECRET, "test_app_secret": TEST_SECRET, "sample_slts": samples}
    if args.state:
        from pathlib import Path
        path = Path(args.state)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(metadata, indent=2))
        path.chmod(0o600)
    print(json.dumps(metadata, indent=2), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
