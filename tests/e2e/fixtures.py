#!/usr/bin/env python3
"""Loopback-only MCP providers (public, bearer, OAuth-protected, desktop-local, stdio) and their OAuth
authorization server, for the end-to-end journey; never part of the app.

Silicon Accounts is faked separately (accounts_fake.py). All credentials in this file are conspicuously fake and
valid only in this process.
"""
import argparse
import base64
import hashlib
import json
import os
import socket
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse


class Fixtures:
    def __init__(self, origin):
        self.origin = origin
        self.lock = threading.RLock()
        self.oauth_codes = {}
        self.oauth_tokens = {}
        self.oauth_exchanges = []
        self.calls = []
        self.requests = []
        self.clients = {}

    def oauth_pair(self, grant, grant_type):
        access = "provider_fixture_" + uuid.uuid4().hex
        refresh = "provider_refresh_fixture_" + uuid.uuid4().hex
        self.oauth_tokens[access] = dict(grant, kind="access", expires=time.time() + grant["lifetime"])
        self.oauth_tokens[refresh] = dict(grant, kind="refresh", expires=time.time() + 3600)
        self.oauth_exchanges.append({key: grant[key] for key in ("account", "family", "generation")} | {"grant_type": grant_type})
        return {"access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": grant["lifetime"], "scope": "mcp:tools"}


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
                return self.send(200, {"calls": self.fixture.calls, "requests": self.fixture.requests, "oauth_exchanges": self.fixture.oauth_exchanges})
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
            # Test-only consent selections: twenty seconds falls inside MCPort's
            # refresh window, so rotation is exercised without sleeping or DB edits.
            if query.get("fixture_account", "oauth-owner") not in ("oauth-owner", "oauth-carbon", "oauth-silicon") or query.get("fixture_token_lifetime", "3600") not in ("20", "3600"):
                return self.error("Unknown fixture OAuth account or token lifetime")
            code = "code_fixture_" + uuid.uuid4().hex
            with self.fixture.lock:
                self.fixture.oauth_codes[code] = dict(query, expires=time.time() + 120)
            destination = redirect + ("&" if "?" in redirect else "?") + urlencode({"code": code, "state": query["state"]})
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
        if path == "/oauth/register":
            client = "client_fixture_" + uuid.uuid4().hex
            self.fixture.clients[client] = body
            return self.send(201, dict(body, client_id=client, client_id_issued_at=int(time.time()), token_endpoint_auth_method="none"))
        if path == "/oauth/token":
            with self.fixture.lock:
                if body.get("grant_type") == "refresh_token":
                    grant = self.fixture.oauth_tokens.get(body.get("refresh_token"))
                    if not grant or grant["kind"] != "refresh" or grant["expires"] < time.time():
                        return self.error("Invalid OAuth refresh token", 401)
                    if body.get("client_id") != grant["client_id"] or body.get("resource") != grant["resource"]:
                        return self.error("Refresh client/resource binding mismatch", 401)
                    # Single-use rotation also invalidates earlier access tokens:
                    # a successful MCP response proves the new bearer was used.
                    self.fixture.oauth_tokens = {token: value for token, value in self.fixture.oauth_tokens.items() if value["family"] != grant["family"]}
                    grant = dict(grant, generation=grant["generation"] + 1)
                elif body.get("grant_type") == "authorization_code":
                    attempt = self.fixture.oauth_codes.pop(body.get("code"), None)
                    if not attempt or attempt["expires"] < time.time():
                        return self.error("Invalid OAuth code", 401)
                    challenge = base64.urlsafe_b64encode(hashlib.sha256(body.get("code_verifier", "").encode()).digest()).decode().rstrip("=")
                    if challenge != attempt["code_challenge"] or body.get("redirect_uri") != attempt["redirect_uri"] or body.get("client_id") != attempt.get("client_id") or body.get("resource") != attempt.get("resource"):
                        return self.error("PKCE/client/redirect/resource binding mismatch", 401)
                    grant = {"account": attempt.get("fixture_account", "oauth-owner"), "client_id": attempt["client_id"], "resource": attempt["resource"], "family": uuid.uuid4().hex, "generation": 0, "lifetime": int(attempt.get("fixture_token_lifetime", "3600"))}
                else:
                    return self.error("Unsupported OAuth grant type", 400)
                tokens = self.fixture.oauth_pair(grant, body["grant_type"])
            return self.send(200, tokens)
        if path.startswith("/mcp/"):
            token = self.headers.get("Authorization", "").removeprefix("Bearer ")
            account = "public" if path == "/mcp/public" else "desktop-owner"
            if path == "/mcp/bearer":
                account = {"fixture-owner-token": "provider-owner", "fixture-silicon-token": "provider-silicon"}.get(token)
                if account is None:
                    return self.send(401, {"error": "invalid_token"}, {"WWW-Authenticate": "Bearer"})
            if path == "/mcp/oauth":
                with self.fixture.lock:
                    grant = self.fixture.oauth_tokens.get(token)
                if not grant or grant["kind"] != "access" or grant["expires"] < time.time():
                    return self.send(401, {"error": "invalid_token"}, {"WWW-Authenticate": 'Bearer resource_metadata="' + self.fixture.origin + '/.well-known/oauth-protected-resource/mcp/oauth"'})
                account = grant["account"]
            try:
                result = mcp_result(body, account, self.fixture)
            except (ValueError, KeyError) as error:
                return self.send(200, {"jsonrpc": "2.0", "id": body.get("id"), "error": {"code": -32601, "message": str(error)}})
            if path == "/mcp/oauth" and body.get("method") == "tools/call" and body.get("params", {}).get("name") == "whoami":
                result["structuredContent"]["token_generation"] = grant["generation"]
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
    parser.add_argument("--state", help="Write the fixture endpoints' URLs (JSON) here")
    args = parser.parse_args()
    if args.stdio:
        return stdio()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    origin = "http://127.0.0.1:" + str(server.server_address[1])
    server.fixture = Fixtures(origin)
    metadata = {"origin": origin, "public_mcp": origin + "/mcp/public", "bearer_mcp": origin + "/mcp/bearer", "oauth_mcp": origin + "/mcp/oauth", "local_mcp": origin + "/mcp/local"}
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
