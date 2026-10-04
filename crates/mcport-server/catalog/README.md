# Community directory attribution

`community.json` derives the server sections of [wong2/awesome-mcp-servers](https://github.com/wong2/awesome-mcp-servers), the public list associated with [mcpservers.org](https://mcpservers.org/). The exact upstream revision, README SHA-256, and skipped malformed row numbers are recorded in the snapshot. Copyright (c) 2024 wong2; the complete MIT license is retained in `LICENSE`.

This is a bundled, attributed snapshot of the repository's list. It is not the entire mcpservers.org website or a live website scrape. Sponsors, clients, frameworks, logos, and copied provider documentation are excluded. Link anchors and query strings are removed; duplicate source links are merged. Descriptions are plain text and entries without reliable setup metadata remain documentation-only. Upstream “Official” grouping is not a MCPort security endorsement.

Only `reviewed-templates.json` supplies endpoint defaults, with provider-owned evidence links. Cloudflare's template connects to its public documentation service, not its account administration servers. GitHub requires each user's provider authorization; a PAT works through MCPort's existing account flow, while OAuth availability depends on the provider's application registration. No credential, environment value, command scraped from prose, or automatic install is included.

To deliberately update the snapshot after reviewing the upstream commit:

```sh
python3 scripts/import-directory.py --revision <40-character-upstream-commit>
```

Review the generated diff and any skipped rows, then run the directory tests. Startup seeds stable global IDs keyed by source link, updates existing metadata in place, and removes absent built-in entries without reusing their IDs. Directory entries are discovery templates; they never modify existing connections. The runtime performs no catalog network fetches.
