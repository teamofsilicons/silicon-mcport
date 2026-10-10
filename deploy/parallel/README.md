# Parallel Accounts production service

The 2026-10-10 release creates an independent Accounts service. Existing IAM Silicons, credentials, workers and resources stay on the old `backend.mcport.teamofsilicons.com` service. No IAM principal is enrolled or adopted. New Accounts resources live only in the new store.

- New public API: `https://api.mcport.teamofsilicons.com`.
- API unit: `mcport-accounts`; configuration `/etc/mcport-accounts/runtime.env` (root0600), data `/var/lib/mcport-accounts`, releases `/opt/mcport-accounts/releases`.
- Accounts runtime secret: `silicon-mcport/accounts-production/runtime`. The webhook secret must come from Accounts; a locally generated placeholder does not configure Accounts deliveries.
- Daily backup: `mcport-accounts-backup.timer`, private manifests under `/var/backups/mcport-accounts`, SSE-encrypted S3 objects under `parallel-accounts-20261010/mcport/backups/` in the existing `silicon-hook-standalone-artifacts-lxpfsbc0jpuk` bucket.

`bootstrap.py mcport` runs as root once, refuses existing namespaces, creates a fresh store/key/configuration and appends only the new API maintenance vhost. It never stops an old writer. The service user needs directory traversal on public executable and certificate directories; environment/key files remain private. Provider credentials are copied only for Waveform's global speech providers; no account records or account encryption keys are reused.

`install.py mcport --archive PATH --sha256 HASH --revision FULL_COMMIT --validator PATH` validates the native backend bundle with the existing repository validator (`deploy/native/upgrade.py` for Waveform; `deploy/install.py` for MCPort), performs a genuine Accounts app credential preflight, stages a separate release, starts only the new API, and opens the new API vhost only after readiness. `--resume-staged` requires the exact selected release and rechecks every staged byte against the candidate before retrying readiness. Never run the legacy in-place installer for this parallel deployment.

The application website uses Accounts and the new API. Existing IAM resources are intentionally absent from this UI and remain available through the unchanged legacy API and Honeycomb CLI. Roll back the website by restoring its saved proxy/project deployment; roll back the new API independently. Do not point the Accounts binary at the legacy store or replace legacy keys.

Backups contain database snapshots plus keys and runtime settings, with no secrets printed. For recovery, stop only the new service, retain a copy of its current files, restore the database with its matching keys/configuration, verify integrity and restore the recorded release. PostgreSQL backups use pg_dump; SQLite backups use the online backup API and an integrity check. The old IAM service is outside these commands.

Production evidence and private receipts are retained in the operator workspace `.migration/live/mcport-accounts`; secrets are intentionally excluded from Git. All production application binaries are optimized release builds. Local debug archives are not release candidates.

MCPort keeps its new SQLite database and independent32-byte `master.key` in `/var/lib/mcport-accounts` and listens at127.0.0.1:4381. The Next.js site is deployed to the existing `silicon-mcport` Vercel project, with server-only Accounts/session secrets and `APP_API_URL=https://api.mcport.teamofsilicons.com`.
