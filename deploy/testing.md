# Isolated acceptance setup

Use a new Honeycomb world for MCPort acceptance. The operator's existing Carbon
session manages that world; MCPort's own production application credential attaches
the application. All acceptance identities and application sessions are created in
the test world. No other application's secret or user token is used for MCPort login.
The sequence below follows the current Honeycomb 0.6.1 and IAM 5.2.1 contracts; it
is a setup procedure, not a claim that a particular live acceptance run passed.

## Register the participant

Append this entry to Honeycomb's `HONEYCOMB_LIFECYCLE_PARTICIPANTS`, preserving its
existing entries:

```json
{"app_id":"mcport","base_url":"https://backend.mcport.teamofsilicons.com","token_env":"MCPORT_LIFECYCLE_SECRET"}
```

Generate an independent service token of at least 32 characters. Store it as
`MCPORT_LIFECYCLE_SECRET` in both runtimes, then restart them. This is deployment
configuration; IAM and Honeycomb source changes are unnecessary. Register before
attachment: without the participant, IAM can import first while Honeycomb remains
pending and does not return the application credential. Resume the same request
after correcting readiness, rather than creating another world.

## Prepare the world and its members

As the existing authorized Honeycomb operator, create a world before importing
MCPort. This provisions the IAM/Honeycomb core without importing application data:

```sh
honeycomb --json environments create tos 'MCPort acceptance' \
  --description 'Dedicated isolated MCPort acceptance'
honeycomb --json environments get "$ENV_ID"
```

Retain the returned UUID and operation identity. Continue only when the core world
is ready. Retrieve its key with `honeycomb environments key "$ENV_ID"`, capturing
the output to a protected file; this command returns credential material.

For IAM CLI onboarding, the same operator needs a direct IAM session authorized
to manage this world. `iam --org tos env key "$ENV_ID"` retrieves and saves the
key for that IAM profile; capture this output privately too. Do not pass a raw key
to IAM's `--test`, which accepts a saved UUID. A fresh IAM profile must authenticate
the operator legitimately before this management call. Alternatively, IAM's
official Rust client accepts the retrieved key through `with_environment`, without
copying a production credential into the test world.

Create the test Carbon and `tos` before attaching the private application. Normal
test signup accepts `000000` and sends no email or SMS. For example:

```sh
iam --test "$ENV_ID" --json signup --email mcport-qa@example.test \
  --carbon-id mcportqa --display-name 'MCPort QA' --timezone Etc/UTC
# Resume the returned signup session using its session ID:
iam --test "$ENV_ID" --json signup --session-id "$SESSION_ID" \
  --email-code 000000 --carbon-id mcportqa \
  --display-name 'MCPort QA' --timezone Etc/UTC
iam --test "$ENV_ID" org create tos --name 'MCPort acceptance'
iam --test "$ENV_ID" --org tos silicon create mcportqa \
  --display-name 'MCPort QA Silicon' --job-description 'Exercise isolated MCPort access'
```

Capture the Silicon creation response privately: it returns its credential once.
Select `tos` when each actor obtains its MCPort SLT through IAM. Creating `tos` first
preserves a login-capable test owner: automatic app import into an empty world
would otherwise create the owning organization with a suspended fixture owner.

## Attach using MCPort's own authority

Honeycomb CLI 0.6.1 supports world creation and operator imports, but has no
app-owned attachment command that returns the app secret. Use its official Rust
client, `Client::with_application("mcport", production_secret)` followed by
`create_application_environment(name, description, Some(root_key), mutation)`.
The matching API contract is:

```text
POST https://backend.honeycomb.teamofsilicons.com/api/v1/environments
Authorization: Basic <base64 of mcport:its-own-production-app-secret>
Content-Type: application/json
Idempotency-Key: <one stable request key>
```

```json
{"name":"MCPort acceptance","description":"Dedicated isolated MCPort acceptance","testing_key":"<prepared-world root key>"}
```

Read secrets from protected files in the operator process, never shell arguments,
logs, or committed files. Do not send a user bearer or `X-Testing-Environment-Key`
on this app-owned control request. It attaches MCPort to the existing world without
transferring ownership. Keep the same request and idempotency key for a retry.

Require the matching environment UUID and `app_id: "mcport"`, `state: "ready"`,
an accepted or unchanged operation, and `credential_state: "ready"`. Save the
returned `app_secret` privately. An ordinary operator import does not return this
credential; app-owned attachment is the credential delivery step. Honeycomb calls
IAM's protected management API internally; operators do not need its service secret.

## Install the test credential and verify

Set `MCPORT_TEST_APP_SECRETS` in the protected runtime configuration to a JSON map
from the world UUID to that returned secret, then restart MCPort. A lifecycle
receipt confirms provisioning, not successful IAM authentication. Credentials can
arrive afterward without replaying import, changing a generation, or rotating the
world key. The map overrides a stored secret only for an already provisioned active
test world. Missing entries use only the same world's stored credential, if any;
missing credentials fail. Unknown, disabled, retired or purged worlds stay blocked.

IAM verifies the selected app credential and world on every authentication path.
An invalid configured credential fails without trying an older or production
credential. Update the entry and restart after test secret rotation or reimport;
retain it until a later lifecycle operation has persisted that credential. Removing
an override does not revoke a credential previously stored in the same world.
Retire the world through Honeycomb to disable testing access.

Authenticate each test actor directly with IAM in this world and obtain an
app-bound MCPort SLT selecting `tos`. In separate fresh MCPort homes, verify both
identities using those issued codes. A world UUID only selects the environment;
MCPort rejects identity selectors such as `c:mcportqa` and `si:mcportqa` as login
credentials, even in testing:

```sh
mcport --test "$ENV_ID" login "$CARBON_MCPORT_SLT"
mcport --test "$ENV_ID" login status --json
# In the Silicon's separate home:
mcport --test "$ENV_ID" login "$SILICON_MCPORT_SLT"
mcport --test "$ENV_ID" login status --json
```

Then configure a test provider, invite the Silicon, execute a useful call, revoke
access, and prove the denial. Check cross-world and production isolation. Use test
provider accounts or fixtures. Current website onboarding has no initial test-world
selector, so these CLI/API checks do not prove browser test-world onboarding.
