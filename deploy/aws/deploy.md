# Production staging and cutover

Provisioned host: `i-0bf2c2f54fce6cfca`, region `us-east-1`, operator profile
`silicon-production`, Elastic IP `100.57.137.244`. Artifact bucket:
`silicon-mcport-production-artifacts-ezytfdmvjxeg`. On October 4, 2026 the native
backend `29fe5b50e4625cda2d3964ed243afe5cba3422f3` was installed through SSM and
passed exact-revision private/public health plus real provider calls. The cutover
preserved runtime secrets, encryption keys and the telemetry spool. The preceding
`66c6e0c` upgrade verified all three transports and acknowledged telemetry; the
latest server-only update corrects report recipients and acceptance terminology.
The new installer handled the existing telemetry socket automatically; no manual
pre-stop or socket removal was needed.

Postmark is configured using the operator-approved sender
`silicon@teamofsilicons.com`. Its credential remains in Secrets Manager and the
protected backend environment. Reports target the two approved Gmail recipients;
provider API acceptance is recorded as `delivery_accepted`, separately from
recipient delivery evidence.

Backend: `https://backend.mcport.teamofsilicons.com`; frontend:
`https://mcport.teamofsilicons.com` on Vercel. Namecheap access was enabled and the
operator IP allowlisted. Only `mcport` → `76.76.21.21` and `backend.mcport` →
`100.57.137.244` were added; all 102 earlier records and mail mode were preserved.
Caddy is active with a valid certificate, and both public origins passed normal
TLS verification. Future installations must also pass genuine public HTTPS
health with the exact candidate revision; do not substitute an IP, disable TLS
verification or rewrite local hosts to bypass that gate.

## Runtime secret

Sign-in needs the `mcport` app in Silicon Accounts (production), its backend-only
`MCPORT_APP_SECRET`, and the app's sign-in setup and webhook described in
[the deploy README](../README.md#silicon-accounts-setup). Provider OAuth uses
backend `/oauth/callback` and `/oauth/client-metadata.json`. Application secrets
never belong in Vercel's browser-visible settings.

Store a JSON object in Secrets Manager secret `silicon-mcport/production-runtime`
using its AWS-managed encryption key. Its only mandatory entry is
`MCPORT_APP_SECRET`, with the real secret as a string. The helper fixes the
nonsecret `ACCOUNTS_URL`, app id, bind, data and origin values; omit unused
optional entries:

| Optional key | When needed |
|---|---|
| `MCPORT_ACCOUNTS_WEBHOOK_SECRET` | The `whsec_` secret of the app webhook at backend `/webhooks/accounts` (sign-outs, removed access, id and custodian changes) |
| `ACCOUNTS_API_URL` | Only when MCPort must call Silicon Accounts at another https URL than `ACCOUNTS_URL` |
| `MCPORT_TELEMETRY_KEY` | Production Space Station table key |
| `POSTMARK_SERVER_TOKEN`, `MCPORT_REPORT_FROM` | Live report delivery and verified sender; sender defaults to `mcport@teamofsilicons.com` |
| `MCPORT_MASTER_KEY` | Optional 64 hex characters, preserved for this deployment's lifetime |

Without an explicit master key the first app startup generates a protected
`master.key` in `/var/lib/mcport`. Keep that directory empty before the first
installer run. The webhook, mail and telemetry are not process-start
prerequisites, but their configured flows must be proved before claiming full
readiness. Blank optional values should be omitted, not copied from examples.
Variables of releases before 0.3.0 (`MCPORT_IAM_URL`, `MCPORT_IAM_WEB_URL`,
`MCPORT_WEBHOOK_SECRET`, `MCPORT_WEBHOOK_SECRET_VERSION`, `MCPORT_LIFECYCLE_SECRET`,
`MCPORT_TEST_APP_SECRETS`, `MCPORT_TEST_TELEMETRY_KEYS`) are ignored with a
warning; remove them when updating an existing runtime (see the cutover runbook).

Create the secret from a protected local JSON file without putting its contents
in shell arguments or SSM documents. Capture registration outputs privately.
Never enable shell tracing, print environment files or show `SecretString`:

```sh
umask 077
aws --profile silicon-production --region us-east-1 secretsmanager create-secret \
  --name silicon-mcport/production-runtime \
  --secret-string file:///protected/mcport-runtime.json \
  > /protected/mcport-secret-receipt.json
```

If it already exists, inspect its metadata and deliberately update that secret;
do not create an unrelated credential. `runtime_from_secret.py` is for the first
configuration only. It captures AWS stdout/stderr, validates the object, and
atomically creates `/etc/mcport/runtime.env` as root0600 without printing values.
It refuses existing files, including symlinks. For later rotation, back up the
matching runtime/database/key first, coordinate service restart and validate sign-in.

## Stage reviewed files through SSM

The operator uploads reviewed scripts and the exact native backend candidate to
immutable `releases/<revision>/` S3 keys. Include `deploy/install.py` from that
same revision and this runtime helper. The instance can read exact release keys;
it cannot list the bucket. Pass only names, object keys and hashes to SSM commands.
Inside the SSM session, use the instance role rather than operator credentials:

```sh
sudo install -d -m 0700 -o root -g root /opt/mcport/bootstrap
# Download exact reviewed object keys with aws s3 cp and verify their SHA-256s.
# Do not run scripts fetched under an unverified mutable key.
sudo python3 /opt/mcport/bootstrap/runtime_from_secret.py
```

Stage Caddy v2.11.7 ARM64 using the pinned archive hash:

```sh
sudo -i
set -eu
umask 077
cd /opt/mcport/bootstrap
curl --fail --location --proto '=https' --tlsv1.2 \
  --output caddy-2.11.7.tar.gz \
  https://github.com/caddyserver/caddy/releases/download/v2.11.7/caddy_2.11.7_linux_arm64.tar.gz
printf '%s  %s\n' d8fc6d179a5d283028a472a5618564f6ad8a86fed513e64f032b3b0b7cc45e42 caddy-2.11.7.tar.gz | sha256sum -c -
tar -xOzf caddy-2.11.7.tar.gz caddy > caddy
install -m 0755 caddy /usr/local/bin/caddy
/usr/local/bin/caddy version
```

Configure a dedicated unprivileged `caddy` user and systemd service using
`/usr/local/bin/caddy run --config /etc/caddy/Caddyfile`, a persistent protected
`/var/lib/caddy` state directory, and only `CAP_NET_BIND_SERVICE`. Do not use
`--environ` or inject MCPort's runtime environment into Caddy. Adapt the reviewed
[`Caddyfile.example`](../Caddyfile.example) to the backend hostname; the frontend
DNS points to the separately verified Vercel target. Keep the backend proxy on
`127.0.0.1:4380`, preserve every `Set-Cookie`, and expose no port4380 listener.

## DNS-gated application installation

After public DNS resolves the backend to its Elastic IP, validate the Caddyfile,
start Caddy and confirm a valid certificate for the exact backend hostname.
Obtain a native backend candidate for the final reviewed implementation revision;
the earlier `1f334d9` candidate does not contain subsequent compact-ID changes.
Set these nonsecret values to that verified candidate before running:

If telemetry has run, its SDK can leave a Unix `daemon.sock` under
`/var/lib/mcport/telemetry/<table-key-hash>/`. The installer recognizes only that
exact path with a 64-character lowercase hexadecimal hash and service-user
ownership. Under its install lock, it stops MCPort, verifies an inactive service
with `MainPID=0`, and removes the ephemeral socket before backup. Spools, cursors,
locks, database, key and runtime files are preserved. Symlinks, changed sockets
and unexpected special files still stop the upgrade. No manual socket cleanup is
needed; the SDK recreates its IPC socket after a new event.

```sh
caddy validate --config /etc/caddy/Caddyfile
systemctl daemon-reload
systemctl enable --now caddy
python3 /opt/mcport/bootstrap/install.py \
  --bundle "$MCPORT_BUNDLE" --sha256 "$MCPORT_BUNDLE_SHA256" \
  --revision "$MCPORT_REVISION"
python3 /opt/mcport/bootstrap/install.py \
  --bundle "$MCPORT_BUNDLE" --sha256 "$MCPORT_BUNDLE_SHA256" \
  --revision "$MCPORT_REVISION" \
  --public-health-url https://backend.mcport.teamofsilicons.com/health --apply
```

The reviewed installer backs up stopped state, switches the immutable release and
requires matching private/public revision health. It starts/enables MCPort only
after the protected runtime exists. Validate actual Carbon/Silicon login, refresh,
provider execution and revocation through both CLI and the Vercel proxy; health
alone does not establish these. Preserve the installer receipt and copy complete
stopped-state backups to unique `backups/` keys before public release. Follow
[native recovery](../README.md) on failure; never restore a database without its
matching encryption key/configuration or over an active writer.
