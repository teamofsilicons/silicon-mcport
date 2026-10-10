import base64
import hashlib
import json
from pathlib import Path
import sys
import threading
import time
import unittest
from urllib.error import HTTPError
from urllib.request import Request, urlopen

sys.path.insert(0, str(Path(__file__).parent))
from fixtures import Fixtures, Handler, ThreadingHTTPServer


class ProtocolFixturesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.origin = "http://127.0.0.1:" + str(cls.server.server_address[1])
        cls.server.fixture = Fixtures(cls.origin)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def request(self, path, value=None, bearer=None, expected=200):
        headers = {}
        if bearer:
            headers["Authorization"] = "Bearer " + bearer
        data = None
        if value is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(value).encode()
        try:
            with urlopen(Request(self.origin + path, data=data, headers=headers)) as response:
                status, result = response.status, json.load(response)
        except HTTPError as error:
            with error:
                status, result = error.code, json.load(error)
        self.assertEqual(status, expected)
        return result

    def test_oauth_rotation_rejects_old_credentials_and_wrong_bindings(self):
        verifier = "fixture-code-verifier"
        resource = self.origin + "/mcp/oauth"
        redirect = self.origin + "/fixture/callback"
        with self.server.fixture.lock:
            self.server.fixture.oauth_codes["fixture-rotation-code"] = {
                "expires": time.time() + 120, "code_challenge": base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("="),
                "client_id": "fixture-client", "redirect_uri": redirect, "resource": resource,
                "fixture_account": "oauth-carbon", "fixture_token_lifetime": "20",
            }
        initial = self.request("/oauth/token", {"grant_type": "authorization_code", "code": "fixture-rotation-code", "code_verifier": verifier, "client_id": "fixture-client", "redirect_uri": redirect, "resource": resource})
        self.assertEqual(initial["expires_in"], 20)
        refresh = {"grant_type": "refresh_token", "refresh_token": initial["refresh_token"], "client_id": "fixture-client", "resource": resource}
        self.request("/oauth/token", dict(refresh, resource=self.origin + "/other"), expected=401)
        self.request("/oauth/token", dict(refresh, client_id="wrong-client"), expected=401)
        self.request("/oauth/token", dict(refresh, refresh_token=initial["access_token"]), expected=401)
        rotated = self.request("/oauth/token", refresh)
        self.assertNotEqual(rotated["refresh_token"], initial["refresh_token"])
        call = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "whoami", "arguments": {}}}
        self.request("/mcp/oauth", call, bearer=initial["access_token"], expected=401)
        self.request("/mcp/oauth", call, bearer=rotated["refresh_token"], expected=401)
        self.request("/oauth/token", refresh, expected=401)
        self.assertEqual(self.request("/mcp/oauth", call, bearer=rotated["access_token"])["result"]["structuredContent"]["token_generation"], 1)
        second = self.request("/oauth/token", dict(refresh, refresh_token=rotated["refresh_token"]))
        self.request("/mcp/oauth", call, bearer=rotated["access_token"], expected=401)
        self.assertEqual(self.request("/mcp/oauth", call, bearer=second["access_token"])["result"]["structuredContent"]["token_generation"], 2)

    def test_mcp_list_pagination_and_structured_results(self):
        first = self.request("/mcp/public", {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})["result"]
        second = self.request("/mcp/public", {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"cursor": first["nextCursor"]}})["result"]
        self.assertIn("nested", [tool["name"] for tool in second["tools"]])
        result = self.request("/mcp/public", {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "nested", "arguments": {"options": {"tags": ["one"]}}}})["result"]
        self.assertEqual(result["structuredContent"]["arguments"]["options"]["tags"], ["one"])
        self.assertEqual(len(result["content"]), 3)


if __name__ == "__main__":
    unittest.main()
