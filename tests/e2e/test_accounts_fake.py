"""Self-tests of the fake Silicon Accounts and its Ed25519 signatures (run with the journey's other fixture tests)."""
import base64
import hashlib
import hmac
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
import threading
import unittest
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

sys.path.insert(0, str(Path(__file__).parent))
import accounts_fake  # noqa: E402
import ed25519  # noqa: E402

SECRET = accounts_fake.DEFAULT_SECRET


class Ed25519Test(unittest.TestCase):
    def test_rfc_8032_vectors(self):
        # RFC 8032 section 7.1, TEST 1 and TEST 2.
        cases = [
            ("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "",
             "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
             "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
            ("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "72",
             "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
             "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
        ]
        for seed, message, public, signature in cases:
            self.assertEqual(ed25519.public_key(bytes.fromhex(seed)).hex(), public)
            self.assertEqual(ed25519.sign(bytes.fromhex(seed), bytes.fromhex(message)).hex(), signature)

    def test_agrees_with_cryptography_when_installed(self):
        try:
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        except ImportError:
            self.skipTest("the cryptography package is not installed")
        for size in (0, 1, 31, 64, 300):
            seed, message = hashlib.sha256(str(size).encode()).digest(), (bytes(range(256)) * 2)[:size]
            self.assertEqual(ed25519.sign(seed, message), Ed25519PrivateKey.from_private_bytes(seed).sign(message))


class Receiver(BaseHTTPRequestHandler):
    """Stands in for MCPort's webhook endpoint."""

    def log_message(self, *_):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        self.server.received.append((dict(self.headers), raw))
        body = b'{"received":true}'
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class FakeAccountsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.servers = []
        cls.fake = ThreadingHTTPServer(("127.0.0.1", 0), accounts_fake.Handler)
        cls.origin = f"http://127.0.0.1:{cls.fake.server_address[1]}"
        cls.fake.fake = accounts_fake.Fake(cls.origin, SECRET)
        cls.receiver = ThreadingHTTPServer(("127.0.0.1", 0), Receiver)
        cls.receiver.received = []
        for server in (cls.fake, cls.receiver):
            threading.Thread(target=server.serve_forever, daemon=True).start()
            cls.servers.append(server)

    @classmethod
    def tearDownClass(cls):
        for server in cls.servers:
            server.shutdown()
            server.server_close()

    def call(self, path, value=None, form=False, app=False, method=None, expected=200):
        headers = {}
        data = None
        if app:
            headers["Authorization"] = "Basic " + base64.b64encode(f"mcport:{SECRET}".encode()).decode()
        if value is not None:
            headers["Content-Type"] = "application/x-www-form-urlencoded" if form else "application/json"
            data = (urlencode(value) if form else json.dumps(value)).encode()
        try:
            with urlopen(Request(self.origin + path, data=data, headers=headers, method=method)) as response:
                status, body = response.status, json.load(response)
        except HTTPError as error:
            with error:
                status, body = error.code, json.load(error)
        self.assertEqual(status, expected, body)
        return body

    def carbon_and_silicon(self, handle):
        carbon = self.call("/fixture/accounts", {"kind": "carbon", "handle": handle})
        silicon = self.call("/fixture/accounts", {"kind": "silicon", "handle": handle + "-si", "custodian": carbon["uuid"]})
        return carbon, silicon

    def claims(self, token):
        payload = token.split(".")[1]
        return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))

    def test_tokens_are_signed_by_the_published_key(self):
        _, silicon = self.carbon_and_silicon("signer")
        slt = self.call("/fixture/slt", {"uuid": silicon["uuid"]})["slt"]
        tokens = self.call("/v1/oauth/token", {"grant_type": "urn:silicon:params:oauth:grant-type:slt", "slt": slt, "client_id": "mcport"}, form=True)
        key = self.call("/.well-known/jwks.json")["keys"][0]
        header_b64, payload_b64, signature_b64 = tokens["access_token"].split(".")
        header = json.loads(base64.urlsafe_b64decode(header_b64 + "=" * (-len(header_b64) % 4)))
        self.assertEqual((header["alg"], header["kid"], key["crv"]), ("EdDSA", key["kid"], "Ed25519"))
        try:
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
        except ImportError:
            Ed25519PublicKey = None
        if Ed25519PublicKey:
            public = Ed25519PublicKey.from_public_bytes(base64.urlsafe_b64decode(key["x"] + "=="))
            public.verify(base64.urlsafe_b64decode(signature_b64 + "=" * (-len(signature_b64) % 4)), f"{header_b64}.{payload_b64}".encode())
        claims = self.claims(tokens["access_token"])
        self.assertEqual((claims["sub"], claims["aud"], claims["kind"], claims["iss"]), (silicon["uuid"], "mcport", "silicon", self.origin))
        self.assertEqual(tokens["account"]["custodian"]["uuid"], silicon["custodian"]["uuid"])
        used = self.call("/v1/oauth/token", {"grant_type": "slt", "slt": slt, "client_id": "mcport"}, form=True, expected=400)
        self.assertIn("already used", used["error_description"])

    def test_device_flow_refresh_rotation_and_reuse(self):
        self.call("/v1/apps/mcport/webhook", {"url": f"http://127.0.0.1:{self.receiver.server_address[1]}/hook"}, app=True, method="PUT")
        carbon, _ = self.carbon_and_silicon("device")
        device = self.call("/v1/device/authorize", {"client_id": "mcport", "client_label": "test"})
        grant = {"grant_type": "urn:ietf:params:oauth:grant-type:device_code", "device_code": device["device_code"], "client_id": "mcport"}
        self.assertEqual(self.call("/v1/oauth/token", grant, form=True, expected=400)["error"], "authorization_pending")
        self.call(f"/fixture/device/{device['user_code']}/approve", {"uuid": carbon["uuid"]})
        first = self.call("/v1/oauth/token", grant, form=True)
        self.assertEqual(self.call("/v1/oauth/token", grant, form=True, expected=400)["error"], "invalid_grant")
        refresh = {"grant_type": "refresh_token", "refresh_token": first["refresh_token"], "client_id": "mcport"}
        second = self.call("/v1/oauth/token", refresh, form=True)
        self.assertTrue(self.call("/v1/oauth/introspect", {"token": second["access_token"]}, form=True, app=True)["active"])
        self.call("/v1/oauth/introspect", {"token": second["access_token"]}, form=True, expected=401)
        reused = self.call("/v1/oauth/token", refresh, form=True, expected=400)
        self.assertIn("already used once", reused["error_description"])
        self.assertFalse(self.call("/v1/oauth/introspect", {"token": second["access_token"]}, form=True, app=True)["active"])
        headers, raw = self.receiver.received[-1]
        event = json.loads(raw)
        self.assertEqual((event["type"], event["data"]["reason"], event["data"]["uuid"]), ("membership.signed_out", "refresh_token_reuse", carbon["uuid"]))
        secret = self.call("/v1/apps/mcport/webhook/generate-secret", {}, app=True)["secret"]
        self.call(f"/fixture/accounts/{carbon['uuid']}/rename", {"display_name": "Renamed"})
        headers, raw = self.receiver.received[-1]
        expected = hmac.new(secret.encode(), headers["X-Accounts-Timestamp"].encode() + b"." + raw, hashlib.sha256).hexdigest()
        self.assertEqual(headers["X-Accounts-Signature"], "v1=" + expected)
        self.assertEqual(json.loads(raw)["data"]["account"]["display_name"], "Renamed")

    def test_lookups_show_the_public_identity_and_the_user_base_the_profile(self):
        carbon, silicon = self.carbon_and_silicon("lookup")
        seen = self.call(f"/v1/accounts/{silicon['uuid']}", app=True)
        self.assertEqual(set(seen), {"uuid", "kind", "id", "status", "custodian"})
        self.assertEqual(self.call(f"/v1/accounts/by-id/{carbon['id']}", app=True)["uuid"], carbon["uuid"])
        self.call(f"/v1/accounts/{carbon['uuid']}", expected=401)
        self.call(f"/v1/apps/mcport/users/{carbon['uuid']}", app=True, expected=404)
        self.call("/fixture/token", {"uuid": carbon["uuid"]})
        member = self.call(f"/v1/apps/mcport/users/{carbon['uuid']}", app=True)
        self.assertEqual((member["display_name"], member["status"]), ("Lookup", "active"))
        self.call(f"/fixture/accounts/{silicon['uuid']}", {}, method="DELETE")
        self.assertEqual(self.call(f"/v1/accounts/{silicon['uuid']}", app=True, expected=404)["error"]["code"], "account_deleted")


if __name__ == "__main__":
    unittest.main()
