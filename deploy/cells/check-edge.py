#!/usr/bin/env python3
"""Exercise the real Caddy header/path bridge using a disposable HTTPS fixture."""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import tempfile
import threading
import time
import urllib.request


class Echo(BaseHTTPRequestHandler):
    def do_GET(self):
        self.respond()

    def do_POST(self):
        self.respond()

    def respond(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        result = json.dumps({"path": self.path,
                             "headers": {k.lower(): v for k, v in self.headers.items()},
                             "body_sha256": hashlib.sha256(body).hexdigest()}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(result)))
        self.end_headers()
        self.wfile.write(result)

    def log_message(self, *args):
        pass


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--caddy", default=shutil.which("caddy"))
    args = parser.parse_args()
    if not args.caddy:
        parser.error("Pass --caddy /absolute/path/to/caddy")
    template = Path(__file__).with_name("Caddyfile.example").read_text()
    env = dict(os.environ, CELLS_MCPORT_HOST="fixture.mcport.test",
               CELLS_MCPORT_PUBLISHABLE_KEY="fixture-publishable-key")
    with tempfile.TemporaryDirectory(prefix="mcport-edge-") as directory:
        root = Path(directory)
        cert, key = root/"cert.pem", root/"key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(key), "-out", str(cert), "-days", "1",
                        "-subj", "/CN=fixture.mcport.test",
                        "-addext", "subjectAltName=DNS:fixture.mcport.test"],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        upstream = ThreadingHTTPServer(("127.0.0.1", 0), Echo)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(cert, key)
        upstream.socket = tls.wrap_socket(upstream.socket, server_side=True)
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        port = free_port()
        config = template.replace("api.mcport.teamofsilicons.com {", f"http://127.0.0.1:{port} {{")
        config = config.replace("reverse_proxy https://{$CELLS_MCPORT_HOST}",
                                f"reverse_proxy https://127.0.0.1:{upstream.server_port}")
        config = config.replace("tls_server_name {$CELLS_MCPORT_HOST}",
                                "tls_server_name {$CELLS_MCPORT_HOST}\n"+
                                "\t\t\ttls_trust_pool file "+str(cert))
        path = root/"Caddyfile"
        path.write_text("{\n\tadmin off\n}\n"+config)
        # Only the test uses a private trust root. The production template keeps
        # ordinary verified public TLS and contains no insecure-skip option.
        with (root/"caddy.log").open("wb") as log:
            process = subprocess.Popen([args.caddy, "run", "--config", str(path),
                                        "--adapter", "caddyfile"], env=env,
                                       stdout=log, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic()+10
                while True:
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                            break
                    except OSError:
                        if process.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError("Caddy fixture startup failed")
                        time.sleep(0.05)
                body = b'{"event":"signed_out","bytes":"\\u0000"}\n'
                headers = {"Authorization": "Bearer genuine-fixture-bearer",
                           "X-Mcport-Authorization": "Bearer forged-fixture-bearer",
                           "apikey": "forged-cell-key", "X-Cells-Identity": "forged-context",
                           "Content-Type": "application/json"}
                request = urllib.request.Request(f"http://127.0.0.1:{port}/webhooks/accounts?x=one%20two",
                                                 data=body, headers=headers)
                with urllib.request.urlopen(request, timeout=5) as response:
                    result = json.load(response)
                received = result["headers"]
                assert result["path"] == "/services/mcport/webhooks/accounts?x=one%20two"
                assert result["body_sha256"] == hashlib.sha256(body).hexdigest()
                assert received.get("x-mcport-authorization") == "Bearer genuine-fixture-bearer"
                assert "authorization" not in received
                assert not any(k.startswith("x-cells-") for k in received)
                assert received["apikey"] == "fixture-publishable-key"
                assert received["host"] == "fixture.mcport.test"
                request = urllib.request.Request(f"http://127.0.0.1:{port}/health",
                                                 headers={"X-Mcport-Authorization": "Bearer forged"})
                with urllib.request.urlopen(request, timeout=5) as response:
                    result = json.load(response)
                assert result["headers"].get("x-mcport-authorization", "") == ""
                assert result["path"] == "/services/mcport/health"
                print(json.dumps({"passed": True, "checks": ["verified upstream TLS", "bearer transport",
                    "spoofed bearer replacement", "missing bearer remains unauthenticated", "cell header removal",
                    "publishable key replacement", "upstream Host", "path and query preservation",
                    "webhook body preservation"]}))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                upstream.shutdown()
                upstream.server_close()


if __name__ == "__main__":
    main()
