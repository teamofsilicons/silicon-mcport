// Test identities on a LOCAL Silicon Accounts stack, for tests/e2e/accounts_stack.py.
//
// It drives the stack's own sign-in pages and dev mail through the Silicon Accounts testkit, so it needs a
// silicon-accounts checkout whose testkit dependencies are installed, and the stack file:
//
//   SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts MCPORT_TEST_STACK=/path/to/test-stack.json \
//     "$SILICON_ACCOUNTS_DIR/testkit/node_modules/.bin/tsx" tests/e2e/stack/mint.mts <command> [options]
//
//   carbon --email E                       a first-party Carbon session (creates the Carbon if new):
//                                          {uuid, id, kind, access_token, refresh_token}
//   app-signin --app A --email E --redirect URI [--exchange] [--scope S]
//                                          the hosted sign-in for app A; with --exchange, the app's tokens
//   silicon --custodian-email E --handle H a Silicon si:H in E's care: {uuid, id, kind, stk, custodian}
//   slt --silicon si:H --stk STK --app A   a single-use, 2-minute short-lived token for app A
//   approve --email E --code CODE          approve a CLI's device sign-in as Carbon E
//
// Every command prints one JSON object. The stack file gives accounts_api_url, mock_messaging_url and the apps'
// development secrets; ACCOUNTS_URL and MOCK_MESSAGING_URL override the URLs. Never point this at a deployed stack.
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = process.env.SILICON_ACCOUNTS_DIR;
const stackFile = process.env.MCPORT_TEST_STACK;
if (!root || !stackFile) {
  console.error('mint: set SILICON_ACCOUNTS_DIR (a silicon-accounts checkout) and MCPORT_TEST_STACK (the stack file)');
  process.exit(2);
}
const kit = await import(pathToFileURL(join(root, 'testkit', 'lib', 'index.ts')).href);
const STACK = JSON.parse(readFileSync(stackFile, 'utf8')) as {
  accounts_api_url: string;
  mock_messaging_url: string;
  apps: Record<string, { app_secret: string }>;
};
const accountsUrl = process.env.ACCOUNTS_URL ?? STACK.accounts_api_url;
if (!/^https?:\/\/(localhost|127\.0\.0\.1)(:\d+)?\/?$/.test(accountsUrl)) {
  console.error(`mint: ${accountsUrl} is not a local stack; this tool only talks to Silicon Accounts on this machine`);
  process.exit(2);
}
const accounts = new kit.AccountsClient(accountsUrl);
const messaging = new kit.MockMessagingClient(process.env.MOCK_MESSAGING_URL ?? STACK.mock_messaging_url);

function arg(name: string, required = true): string {
  const i = process.argv.indexOf(`--${name}`);
  const v = i >= 0 ? process.argv[i + 1] : undefined;
  if (required && !v) {
    console.error(`mint: --${name} is required`);
    process.exit(2);
  }
  return v ?? '';
}
const flag = (name: string) => process.argv.includes(`--${name}`);
const out = (v: unknown) => console.log(JSON.stringify(v));

function secretOf(app: string): string {
  const s = STACK.apps[app]?.app_secret;
  if (!s) {
    console.error(`mint: the stack file has no secret for app '${app}'`);
    process.exit(2);
  }
  return s;
}

async function carbonSession(email: string) {
  try {
    const { tokens, session } = await accounts.cliLogin(messaging, { email });
    return { tokens, session, me: await session.me() };
  } catch {
    const created = await kit.signUpCarbon({ accounts, messaging, email });
    const { tokens, session } = await accounts.cliLogin(messaging, { email });
    return { tokens, session, me: created.me };
  }
}

const cmd = process.argv[2];
if (cmd === 'carbon') {
  const { tokens, me } = await carbonSession(arg('email'));
  out({ uuid: me.uuid, id: me.id, kind: 'carbon', access_token: tokens.access_token, refresh_token: tokens.refresh_token });
} else if (cmd === 'app-signin') {
  const app = arg('app');
  const email = arg('email');
  await carbonSession(email); // the Carbon exists before the hosted pages run
  const scope = flag('scope') ? arg('scope') : undefined;
  const r = await kit.signInWithCode({ accounts, messaging, appId: app, email, redirectUri: arg('redirect'), ...(scope ? { scope } : {}) });
  if (!flag('exchange')) {
    out({ code: r.code, code_verifier: r.codeVerifier, state: r.state, redirect_uri: r.redirectUri });
  } else {
    if (!r.code) throw new Error(`sign-in ended without a code: ${r.error}`);
    out({ tokens: await accounts.app(app, secretOf(app)).exchangeCode(r.code, r.redirectUri, r.codeVerifier) });
  }
} else if (cmd === 'silicon') {
  const { session, me } = await carbonSession(arg('custodian-email'));
  const handle = arg('handle').replace(/^si:/, '');
  const created = await session.createSilicon({ id: `si:${handle}`, display_name: handle }, randomUUID());
  out({ uuid: created.silicon.uuid, id: created.silicon.id, kind: 'silicon', stk: created.stk, custodian: { uuid: me.uuid, id: me.id } });
} else if (cmd === 'slt') {
  const tokens = await accounts.siliconLogin(arg('silicon'), arg('stk'), 'mcport e2e');
  const res = await fetch(`${accounts.url}/v1/me/short-lived-tokens`, {
    method: 'POST',
    headers: { authorization: `Bearer ${tokens.access_token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ app_id: arg('app') }),
  });
  const body = await res.json();
  if (!res.ok) throw new Error(`short-lived token refused: ${res.status} ${JSON.stringify(body)}`);
  out(body);
} else if (cmd === 'approve') {
  const { tokens } = await carbonSession(arg('email'));
  const code = arg('code').toUpperCase();
  const res = await fetch(`${accounts.url}/v1/device/${encodeURIComponent(code)}/approve`, {
    method: 'POST',
    headers: { authorization: `Bearer ${tokens.access_token}` },
  });
  if (!res.ok) throw new Error(`device approval refused: ${res.status} ${await res.text()}`);
  out({ approved: code, status: res.status });
} else {
  console.error('usage: mint.mts carbon|app-signin|silicon|slt|approve (see the header of this file)');
  process.exit(2);
}
