# Directory and access defaults — development verification

This change adds a bundled community directory, organization entries, template-based connection creation, and org/invite-only access defaults. It has not been deployed or included in the public Honeycomb release.

Verified on 2026-10-04 using isolated IAM/provider fixtures and actual gateway/CLI binaries:

- 91 end-to-end checks passed, including directory search, creator-only edits, cross-org denial, template review without creation, actual tool invocation from a directory-created connection, and unchanged connections after template edit/deletion.
- No-auth creation defaults to org access; both provider-account modes default to invited. Invite-only starts owner-only. Explicit legacy owner-only resets revoke existing invitations.
- Raw API callers receive the same defaults. Conflicting directory edits return 409. Source URLs containing credentials are rejected.
- Testing entries cannot be read in production; lifecycle cleanup deletes them while preserving the bundled catalog.
- 56 server tests passed, including atomic migration of old private connections without activating dormant invites, rollback on a stale update, and stable catalog IDs on restart. Existing credentials and tool policies are retained.
- Rust workspace tests and strict Clippy passed. The web's 38 tests and production build passed. Packaging/import checks (28 tests) and fixture tests (4 tests) passed.

Reproduce with `python3 tests/e2e/run.py`; the retained local result is `.local/e2e/directory-20261004-2/result.json`. Packaging tests use the declared `scripts/requirements.txt` dependency in an isolated virtual environment.

Manual browser verification remains pending: browser access to the isolated local test website was declined. No alternative browser surface was used to bypass that decision. These checks establish CLI/API integration and automated web behavior, not a completed manual website journey or a production release.

The source is the MIT-licensed public Awesome MCP Servers repository at `39eb1d76d76e562657f3c657266a504cea159e23`: 508 imported entries, two explicitly recorded malformed rows skipped, and reviewed remote templates for Cloudflare Documentation and GitHub. This is not a live mirror of every mcpservers.org website entry. See the [catalog provenance](../../../crates/mcport-server/catalog/README.md).
