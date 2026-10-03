# MCPort end-to-end fixtures

Run the real server and CLI through official IAM SDK wire protocols and actual HTTP/stdio MCP transports:

```sh
python3 -m unittest discover -s tests/e2e -p 'test_*.py'
python3 tests/e2e/run.py
```

`run.py` builds the binaries, allocates separate loopback ports and temporary data/homes, runs the regression journey, then stops its own daemons and child services. `--no-build` reuses binaries. `--run-dir /new/empty/directory` retains logs and `result.json` at a chosen location. Use a fresh directory per run. No server authentication bypass or direct database mutation is involved. The deletion regression reads only non-secret SQLite record indexes to verify that API-created provider grants, epochs, invites, policies and pending OAuth attempts were removed.

Coverage includes Carbon/Silicon and outsider identities; ordinary-member connection creation; private/org/invited access; pagination and schemas; inline/file/stdin inputs; mixed media; current-policy full-result reads and downloads, cancellation response redaction and no-clobber files; connection deletion authority cleanup; shared and per-user provider grants; OAuth PKCE/browser consent; local HTTP and stdio calls from another home; disconnect/offline/reconnect; idempotency and lost-response no-replay; cancellation; IAM revocation; isolated Honeycomb prepare/disable/restore/clean and receipt replay; telemetry opt-out; durable bug-report status with no live email.

For exploratory manual testing, start:

```sh
python3 tests/e2e/serve.py
```

This prints the CLI path, URLs and sample **fixture-only** SLTs. Its default ports are backend 4380 and IAM/MCP 4390. Choose different ports with `--backend-port` and `--fixture-port` if those are in use. Use distinct existing `SILICON_HOME` directories for `owner`, `silicon`, `stranger` and `crossorg`, and set `MCPORT_URL` to the printed backend. SLTs expire and are consumed once; mint another with fixture-only `POST /fixture/slt` JSON `{"role":"owner"}`. `POST /fixture/revoke` supports `{"principal_id":"si:researcher","revoked":true}`. Both endpoints exist only in the fixture process.

The fixture implements `/api/v1/app-auth/tokens`, introspection and application testing context with the actual SDK headers, Basic application auth and form bodies. Test environments must first be provisioned through the backend's authenticated Honeycomb lifecycle endpoint; supplying a test header never provisions one. HTTP services include public, bearer-protected, OAuth-protected and desktop-local endpoints; `fixtures.py --stdio` serves the same fixture over newline JSON-RPC.

Fixture runners remove inherited Postmark and MCPort telemetry keys. Mail delivery retry/restart is tested separately against an in-process loopback Postmark fixture in `operations::tests`; test-environment reports never reach that worker. These tests prove integration behavior with controlled providers, not compatibility with every live provider or a production deployment.
