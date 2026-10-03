import base64
import hashlib
import json
from pathlib import Path
import sys
import threading
import unittest
from urllib.parse import urlencode
from urllib.request import Request, urlopen

sys.path.insert(0, str(Path(__file__).parent))
from fixtures import Fixtures, Handler, TEST_ID, TEST_KEY, ThreadingHTTPServer


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

    def request(self, path, value=None, iam=False, test=False):
        headers = {}
        if iam:
            secret = "fixture-test-app-secret" if test else "fixture-app-secret"
            headers = {"Authorization": "Basic " + base64.b64encode(("mcport:" + secret).encode()).decode(), "Silicon-IAM-Supported-API-Versions": "v1", "Idempotency-Key": "fixture-" + str(id(value)) + "-key"}
            if test:
                headers["X-Testing-Environment-Key"] = TEST_KEY
        data = None
        if value is not None:
            headers["Content-Type"] = "application/x-www-form-urlencoded" if iam else "application/json"
            data = (urlencode(value) if iam else json.dumps(value)).encode()
        with urlopen(Request(self.origin + path, data=data, headers=headers)) as response:
            return json.load(response)

    def test_official_application_exchange_and_current_authorization(self):
        slt = self.request("/fixture/slt", {"role": "owner"})["slt"]
        tokens = self.request("/api/v1/app-auth/tokens", {"app_id": "mcport", "slt": slt}, iam=True)
        actor = self.request("/api/v1/oauth/introspect", {"token": tokens["access_token"]}, iam=True)
        self.assertEqual(actor["authorization"]["public_id"], "c:owner")
        self.assertEqual(actor["authorization"]["org_role"], "member")
        self.request("/fixture/revoke", {"principal_id": "c:owner"})
        self.assertFalse(self.request("/api/v1/oauth/introspect", {"token": tokens["access_token"]}, iam=True)["active"])
        self.request("/fixture/revoke", {"principal_id": "c:owner", "revoked": False})

    def test_testing_context_requires_matching_test_credential(self):
        context = self.request("/api/v1/application/testing-context", iam=True, test=True)
        self.assertEqual(context["environment_id"], TEST_ID)
        slt = self.request("/fixture/slt", {"role": "silicon", "environment": TEST_ID})["slt"]
        tokens = self.request("/api/v1/app-auth/tokens", {"app_id": "mcport", "slt": slt}, iam=True, test=True)
        actor = self.request("/api/v1/oauth/introspect", {"token": tokens["access_token"]}, iam=True, test=True)
        self.assertEqual(actor["authorization"]["testing_environment_id"], TEST_ID)

    def test_mcp_list_pagination_and_structured_results(self):
        first = self.request("/mcp/public", {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})["result"]
        second = self.request("/mcp/public", {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"cursor": first["nextCursor"]}})["result"]
        self.assertIn("nested", [tool["name"] for tool in second["tools"]])
        result = self.request("/mcp/public", {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "nested", "arguments": {"options": {"tags": ["one"]}}}})["result"]
        self.assertEqual(result["structuredContent"]["arguments"]["options"]["tags"], ["one"])
        self.assertEqual(len(result["content"]), 3)


if __name__ == "__main__":
    unittest.main()
