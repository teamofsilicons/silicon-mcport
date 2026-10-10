# Proposed changes to MCPort's UNDERSTANDING.md

`understanding/UNDERSTANDING.md` is changed only by Carbons, so this migration did not edit it. Below is replacement
text for the sections that describe Silicon IAM, organizations, Honeycomb and testing environments, written at the same
product level. Sections not listed stay as they are. Later migration stages may refine the CLI and website parts.

## Replace the first paragraph's last sentence (IDs)

MCPort’s own public IDs start at 3 characters using `a–z`, `A–Z` and `0–9`. Use all available combinations before
increasing to 4 characters, then 5 and so on. IDs stay unique and are never reused after deletion. Silicon Accounts
identities and authentication tokens keep their own formats.

## Login (replace the section)

Sign-in is handled by Silicon Accounts, using its official client. Carbons and Silicons have the same capabilities under
the same permissions. Every Carbon and Silicon has one personal account: MCPort keys everything on the account's
permanent uuid and shows its current `c:`/`si:` id.

Carbons sign in to the CLI with the device flow (`mcport login` shows a code to confirm in Silicon Accounts) and to the
website through Silicon Accounts' pages. Silicons never see a page: they get a short-lived token for MCPort from
Silicon Accounts and hand it to `mcport login`. The app secret stays on the backend.

Everything belongs to personal accounts. A Silicon always has a custodian, the Carbon who looks after it. Several
identities on one machine use separate homes.

## Configuring MCPs (replace the first two paragraphs)

Any Carbon or Silicon can create a connection and becomes its owner.

A connection has a name, description, owner and configuration. It can use cloud HTTP, local HTTP like Figma desktop,
or a local stdio process with its command, arguments and environment settings.

## Sharing and Access (replace the first paragraph)

A connection is private to its owner, shared with the owner's circle, or shared with selected Carbons and Silicons by
their ids. A Carbon's circle is the Carbon and the Silicons it looks after; a Silicon's circle is the Silicon, its
custodian and the custodian's other Silicons. The custodian of a Silicon sees and manages that Silicon's connections,
hosts, directory entries and activity, but never acts as the Silicon.

Silicons are not open to the world: sharing with a Silicon outside your circle needs that Silicon, or its custodian, to
have allowed you first. Carbons can be reached by anyone signed in.

(Keep the remaining paragraphs; in "Revocation and membership changes block subsequent calls", read "Sign-outs, removed
access and sharing changes block subsequent calls".)

## Using the Application (replace the first sentence)

The website and CLI list connections I own, connections of the Silicons I look after, my circle's connections and those
shared with me.

## Honeycomb (replace the section with "Silicon Accounts and Silicon Apps")

Follow the Silicon Accounts and Silicon Apps developer guides (developers.teamofsilicons.com). Silicon Accounts handles
identity, custodians and consent; Silicon Apps distributes and updates the CLI; this app owns connections and access.

Request only needed account data. Verify signed webhooks, handle duplicate and older events, and act on sign-outs and
removed access. Calls from other apps would use Silicon Accounts proofs with declared scopes; MCPort accepts none yet.
Proofs never replace connection permissions or provider authentication.

Testing uses a separate MCPort backend connected to a test Silicon Accounts deployment, with test accounts and fixture
MCPs; nothing falls back to production.

Ship documented, validated packages for the Silicon Apps targets. Verify fresh installation, both identity types, both
account modes, remote local-MCP use, restrictions, revocation and the first useful command.

## Rust Package & CLI (replace two sentences)

- "Support Carbons and Silicons in their authorized organization contexts through IAM." → "Support Carbons and Silicons
  signed in through Silicon Accounts."
- "Keep accounts and test environments separate even when sharing a daemon." → "Keep accounts separate even when
  sharing a daemon."

## CLI Experience (replace the first two paragraphs and the required commands)

`mcport login` signs a Carbon in with the device flow. A Silicon signs in with a short-lived token from Silicon Accounts:
`silicon-accounts login --app mcport -q | mcport login --slt-stdin` (or `mcport login --slt <token>`). MCPort's CLI and
package never collect Silicon Accounts passwords or keys. Upstream MCP authentication is a separate operation.

Required commands:

- `mcport accounts --json` returns `app_id` and sign-in details before login, offline.
- `mcport login status --json` returns `authenticated: true` after sign-in with the account's uuid, id and kind, and
  `authenticated: false` before.
- `mcport --help` and `mcport -h` show the command tree. Every branch and command has its own help.

(Remove the paragraph about `--test`.)

## CLI Examples

- Install: `silicon-apps install mcport`, then `mcport accounts --json`, `mcport login`, `mcport login status --json`.
- `--visibility org` becomes `--visibility circle`; `access new … --principal "si:researcher"` becomes
  `--account "si:researcher"`.
- Replace "Change Access and Use Testing" with "Change access": disable a tool for everyone or one account
  (`--account`), remove access, share with your circle (`mcport connection set docs --visibility circle`), and allow an
  account to share with a Silicon you look after (`mcport allow add c:ada --silicon si:researcher`).
- "Connection names resolve within the selected organization" → "Connection names resolve among the connections you can
  use, your own first".

## CLI stage additions (2026-10-10)

- CLI Experience, after the required commands: "`mcport logout` ends this machine's sign-in at Silicon Accounts; other
  machines and the website stay signed in. One home keeps one sign-in per backend; several identities on one machine use
  separate homes."
- Rust Package & CLI: "The Rust package signs in with Silicon Accounts as MCPort's public client (device flow and
  short-lived tokens) and keeps a stored sign-in with single-flight refresh, so the CLI has no capability the package
  lacks."
- Local MCPs: "Hosts registered before the move to Silicon Accounts keep serving their connections; their owner runs
  `mcport host migrate <host>` once on the host to re-key its local provider accounts by account."
- Sharing and Access: "A Silicon, or its custodian, chooses who outside its own people may share with it (`mcport allow
  add <c:/si: id>`)."
- CLI Examples: every `--principal` becomes `--account` (for example `mcport tool set docs delete --enabled false
  --account si:researcher`).
