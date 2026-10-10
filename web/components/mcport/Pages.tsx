"use client";
import { useEffect, useState } from "react";
import {
  Activity as ActivityIcon,
  ArrowRight,
  BookOpen,
  ExternalLink,
  Monitor,
  Plus,
  RefreshCw,
  ShieldCheck,
  Terminal,
} from "lucide-react";
import { api, message } from "./lib/api";
import { HoldToConfirm } from "@/components/arc/hold-to-confirm/hold-to-confirm";
import { McpResult } from "./McpResult";
import type { Activity, Discovery, Host, Session } from "./lib/api";
import {
  Button,
  Code,
  Dialog,
  DialogContent,
  Empty,
  ErrorBox,
  Input,
  Loading,
  Notice,
  PageHead,
  Status,
  Switch,
  Textarea,
} from "./ui";
const time = (v?: number) =>
  v
    ? new Date(v * 1000).toLocaleString(undefined, {
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      })
    : "Never";
export function HostsPage() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [show, setShow] = useState(false);
  const load = () => {
    setLoading(true);
    setError("");
    api
      .hosts()
      .then(setHosts)
      .catch((e) => setError(message(e)))
      .finally(() => setLoading(false));
  };
  useEffect(load, []);
  return (
    <>
      <PageHead
        eyebrow="LOCAL, CONNECTED"
        title="Hosts"
        description="The machines that bring your local MCPs to your whole workspace."
        action={
          <Button onClick={() => setShow(true)}>
            <Plus size={16} />
            Register a host
          </Button>
        }
      />
      <div className="host-info">
        <span className="metric-icon peach">
          <Monitor size={22} />
        </span>
        <div>
          <strong>Your tools stay on your machine.</strong>
          <p>
            The MCPort daemon connects outward. Authorized Carbons and Silicons
            can call its registered MCPs from anywhere.
          </p>
        </div>
        <ShieldCheck size={22} />
      </div>
      <div className="section-heading">
        <h2>Your hosts</h2>
        <Button size="sm" variant="secondary" loading={loading} onClick={load}>
          <RefreshCw size={14} />
          Refresh
        </Button>
      </div>
      {error ? (
        <ErrorBox error={error} onRetry={load} />
      ) : loading ? (
        <Loading />
      ) : hosts.length ? (
        <div className="host-grid">
          {hosts.map((h) => (
            <div className="host-card" key={h.id}>
              <div className="card-top">
                <span className="connection-icon peach">
                  <Monitor size={23} />
                </span>
                <Status status={h.online ? "online" : "offline"} />
              </div>
              <h3>{h.name}</h3>
              <p>Last connected {time(h.last_seen)}</p>
              <code>{h.id}</code>
              <small>{h.owner.id||h.owner.uuid}</small>
              {h.can_manage&&<HoldToConfirm tone="danger" label={"Hold to remove "+h.name} onConfirm={()=>{void api.deleteHost(h.id).then(load).catch(e=>setError(message(e)));}}/>}
              <div className="host-card-footer">
                <ShieldCheck size={14} />
                Only registered endpoints can run
              </div>
            </div>
          ))}
        </div>
      ) : (
        <Empty
          title="Connect your first machine"
          description="Register the computer running your desktop MCP or stdio process. The daemon keeps it available while it’s online."
          icon={<Monitor size={25} />}
          action={
            <Button onClick={() => setShow(true)}>
              Set up a host <ArrowRight size={16} />
            </Button>
          }
        />
      )}
      <Dialog open={show} onOpenChange={setShow}>
        <DialogContent
          title="Connect a local host"
          description="Run these commands on the machine where your MCP lives."
        >
          <div className="form-stack">
            <ol className="setup-list">
              <li>
                <span>1</span>
                <div>
                  <strong>Install and sign in</strong>
                  <p>Sign in with Silicon Accounts on this machine.</p>
                  <Code>
                    {'silicon-apps install mcport\nmcport login'}
                  </Code>
                </div>
              </li>
              <li>
                <span>2</span>
                <div>
                  <strong>Register this machine</strong>
                  <p>The CLI registers the host and starts its daemon.</p>
                  <Code>{"mcport host new my-mac"}</Code>
                </div>
              </li>
              <li>
                <span>3</span>
                <div>
                  <strong>Add your local MCP</strong>
                  <p>Create a local connection here or use the CLI.</p>
                  <Code>
                    {
                      "mcport connection new figma --host my-mac --transport http \\\n  --url http://127.0.0.1:3845/mcp --auth shared --visibility invited"
                    }
                  </Code>
                </div>
              </li>
            </ol>
            <Notice>
              Keep your host online and the MCP running. No public inbound port
              is required.
            </Notice>
            <Button
              onClick={() => {
                setShow(false);
                load();
              }}
            >
              Done — refresh hosts <RefreshCw size={15} />
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
export function ActivityPage({
  callId,
  onSelect,
}: {
  callId?: string;
  onSelect: (id?: string) => void;
}) {
  const [items, setItems] = useState<Activity[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Activity | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState("");
  const [busy, setBusy] = useState(false);
  const load = async (quiet = false) => {
    if (!quiet) setLoading(true);
    try {
      setItems(await api.activity());
      setError("");
    } catch (e) {
      setError(message(e));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(true), 5000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    if (!callId) {
      setSelected(null);
      return;
    }
    let active = true;
    setSelected(null);
    setDetailLoading(true);
    setDetailError("");
    const read = async () => {
      try {
        const call = await api.invocation(callId);
        if (active) {
          setSelected(call);
          setDetailError("");
        }
      } catch (e) {
        if (active) {
          setSelected(null);
          setDetailError(message(e));
        }
      } finally {
        if (active) setDetailLoading(false);
      }
    };
    void read();
    const timer = setInterval(() => void read(), 5000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [callId]);
  async function cancel(id: string) {
    setBusy(true);
    try {
      setSelected(await api.cancel(id));
      await load(true);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <PageHead
        eyebrow="EVERY CALL, ACCOUNTED FOR"
        title="Activity"
        description="Your calls, execution accounts, and outcomes. Automatically refreshed."
        action={
          <Button
            variant="secondary"
            loading={loading}
            onClick={() => void load()}
          >
            <RefreshCw size={15} />
            Refresh
          </Button>
        }
      />
      {error && <ErrorBox error={error} />}{" "}
      {loading && !items.length ? (
        <Loading />
      ) : items.length ? (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Action</th>
                <th>Connection</th>
                <th>Account used</th>
                <th>Outcome</th>
                <th>When</th>
              </tr>
            </thead>
            <tbody>
              {items.map((a) => (
                <tr key={a.id}>
                  <td>
                    <button
                      className="activity-action"
                      onClick={() => onSelect(a.id)}
                    >
                      <span className="table-icon">
                        <Terminal size={15} />
                      </span>
                      <span>
                        <strong>{a.tool_name || a.method}</strong>
                        <small>{a.caller.id}</small>
                      </span>
                    </button>
                  </td>
                  <td>{a.connection_name}</td>
                  <td>
                    <code>
                      {a.execution_account?.id || "No provider account"}
                    </code>
                  </td>
                  <td>
                    <Status status={a.status} />
                  </td>
                  <td>{time(a.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        !error && (
          <Empty
            title="Your first call starts here"
            description="Run a tool from the website or CLI. Its caller, account, progress, and outcome will appear here."
            icon={<ActivityIcon size={26} />}
          />
        )
      )}
      <Dialog
        open={!!callId}
        onOpenChange={(v) => {
          if (!v) onSelect();
        }}
      >
        <DialogContent
          title={selected?.tool_name || selected?.method || "Call details"}
          description="Call history is visible only to its initiating actor."
        >
          {detailLoading && <Loading />}
          {detailError && <ErrorBox error={detailError} />}
          {selected && (
            <div className="form-stack">
              <Status status={selected.status} />
              <dl className="property-list">
                <div>
                  <dt>Call ID</dt>
                  <dd>
                    <code>{selected.id}</code>
                  </dd>
                </div>
                <div>
                  <dt>Caller</dt>
                  <dd>{selected.caller.id}</dd>
                </div>
                <div>
                  <dt>Account</dt>
                  <dd>{selected.execution_account?.id}</dd>
                </div>
                <div>
                  <dt>Started</dt>
                  <dd>{time(selected.created_at)}</dd>
                </div>
              </dl>
              {selected.error && (
                <ErrorBox
                  error={[
                    selected.error.message,
                    selected.error.recovery,
                    selected.error.outcome_unknown
                      ? "The action may have completed. Check the provider before retrying."
                      : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                />
              )}{" "}
              {selected.result !== null && selected.result !== undefined && (
                <McpResult result={selected.result} callId={selected.id} />
              )}
              {["pending", "queued", "running", "dispatched"].includes(
                selected.status,
              ) && (
                <Button
                  variant="danger"
                  loading={busy}
                  onClick={() => void cancel(selected.id)}
                >
                  Cancel pending work
                </Button>
              )}
              <p className="field-help">
                Cancellation is best effort and cannot undo actions already
                completed by the provider.
              </p>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
export function SettingsPage({
  session,
  discovery,
  notify,
}: {
  session: Session;
  discovery: Discovery | null;
  notify: (s: string) => void;
}) {
  const [telemetry, setTelemetry] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState("");
  const [pr, setPr] = useState("");
  const [reporting, setReporting] = useState(false);
  useEffect(() => {
    api
      .settings()
      .then((v) => setTelemetry(v.telemetry))
      .catch((e) => setError(message(e)));
  }, []);
  async function toggle(v: boolean) {
    setBusy(true);
    try {
      const r = await api.setSettings(v);
      setTelemetry(r.telemetry);
      notify(`Telemetry ${r.telemetry ? "enabled" : "disabled"}.`);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function send() {
    setReporting(true);
    setError("");
    try {
      const r = await api.report(report.trim(), pr.trim() || undefined);
      setReport("");
      setPr("");
      notify(`Report ${r.status}. Reference: ${r.id}`);
    } catch (e) {
      setError(message(e));
    } finally {
      setReporting(false);
    }
  }
  return (
    <>
      <PageHead
        eyebrow="MAKE IT YOURS"
        title="Settings"
        description="Your account, privacy preferences, and ways to make MCPort better."
      />
      <div className="narrow-section">
        {error && <ErrorBox error={error} />}
        <section className="settings-section">
          <h2>Account</h2>
          <dl className="property-list">
            <div>
              <dt>Identity</dt>
              <dd>
                {session.display_name}
                <span className="small-pill">
                  {session.kind}
                </span>
              </dd>
            </div>
            <div>
              <dt>Public ID</dt>
              <dd>
                <code>{session.id}</code>
              </dd>
            </div>
            <div><dt>Account UUID</dt><dd>{session.uuid}</dd></div>

          </dl>
        </section>
        <section className="settings-section">
          <h2>Privacy & diagnostics</h2>
          <div className="setting-row">
            <div>
              <h3>Operational telemetry</h3>
              <p>
                Help diagnose failures across the website, CLI, daemon, and
                backend. Secrets, raw tool inputs, results, and file contents
                are excluded.
              </p>
            </div>
            {telemetry === null ? (
              <Loading />
            ) : (
              <Switch
                aria-label="Operational telemetry"
                checked={telemetry}
                disabled={busy}
                onCheckedChange={(v) => void toggle(v)}
              />
            )}
          </div>
          <p className="field-help">
            CLI preference: <code>mcport config set telemetry false</code>
          </p>
        </section>
        <section className="settings-section">
          <h2>Report a problem</h2>
          <p className="muted-copy">
            Explain what happened and how to reproduce it. Leave out tokens and
            private provider data.
          </p>
          <form
            className="form-stack"
            onSubmit={(e) => {
              e.preventDefault();
              void send();
            }}
          >
            <Textarea
              label="What went wrong?"
              value={report}
              onChange={setReport}
              placeholder="What did you try, what did you expect, and what happened?"
            />
            <Input
              label="Pull request (optional)"
              type="url"
              value={pr}
              onChange={(e) => setPr(e.target.value)}
              placeholder="https://github.com/…/pull/…"
            />
            <Button type="submit" loading={reporting} disabled={!report.trim()}>
              Submit bug report <ArrowRight size={15} />
            </Button>
          </form>
          {discovery?.repository_url && (
            <a
              className="text-button"
              href={discovery.repository_url}
              target="_blank"
              rel="noreferrer"
            >
              MCPort is open source. Explore the repository{" "}
              <ExternalLink size={14} />
            </a>
          )}
        </section>
        <section className="settings-section">
          <h2>Built with care</h2>
          <p className="muted-copy">
            MCPort uses open source components from{" "}
            <a href="https://uiarc.dev" target="_blank" rel="noreferrer">
              Arc UI
            </a>
            .{" "}
            <a href="/UI-ARC-LICENSE.txt" target="_blank" rel="noreferrer">
              MIT license
            </a>
            .
          </p>
        </section>
      </div>
    </>
  );
}
export function HelpPage({ discovery }: { discovery: Discovery | null }) {
  return (
    <>
      <PageHead
        eyebrow="FROM SETUP TO FIRST CALL"
        title="A little setup. A lot of possibility."
        description="Use the same connections and permissions in your browser and terminal."
      />
      <div className="help-layout">
        <div>
          <section className="help-section">
            <span className="step-label">01 / GET CONNECTED</span>
            <h2>Install and sign in</h2>
            <p>
              Install MCPort with Silicon Apps. Carbons use the hosted sign-in; Silicons pipe a short-lived token from Silicon Accounts into MCPort.
            </p>
            <Code>
              {
                'silicon-apps install mcport\nmcport discovery --json\nmcport login\nmcport login status --json'
              }
            </Code>
          </section>
          <section className="help-section">
            <span className="step-label">02 / ADD YOUR MCP</span>
            <h2>Give your tools a home</h2>
            <p>
              Create an cloud connection. The saved connection
              holds its endpoint and account settings, so you don’t repeat them
              with every call.
            </p>
            <Code>
              {
                'mcport connection new docs --transport http \\\n  --url "https://mcp.example.com/mcp" \\\n  --auth none --visibility circle\n\nmcport connection ls --json'
              }
            </Code>
            <p className="field-help">
              The URL and tool names here are examples. Use your provider’s
              actual MCP endpoint.
            </p>
          </section>
          <section className="help-section">
            <span className="step-label">03 / MAKE IT USEFUL</span>
            <h2>Discover, inspect, run</h2>
            <p>
              Look at the tools and their input schemas first. Pass a JSON
              object directly, from a file, or through stdin.
            </p>
            <Code>
              {
                'mcport tool ls docs --json\nmcport tool show docs search --json\nmcport tool call docs search --input \'{"query":"release notes"}\' --json\n\nmcport tool call docs search --input @query.json --json\ncat query.json | mcport tool call docs search --input - --json'
              }
            </Code>
          </section>
          <section className="help-section">
            <span className="step-label">04 / SHARE WITH INTENT</span>
            <h2>The right tools for the right people</h2>
            <p>
              Share with selected Carbons or Silicons, or open a connection to
              your circle. Tool permissions start enabled and can be
              restricted individually.
            </p>
            <Code>
              {
                'mcport connection set docs --visibility invited\nmcport access new docs --principal "si:researcher"\nmcport tool set docs delete --enabled false\nmcport access rm docs --principal "si:researcher"'
              }
            </Code>
          </section>
        </div>
        <aside className="help-aside">
          <div className="panel">
            <BookOpen size={23} />
            <h3>Three things to choose</h3>
            <div className="help-fact">
              <strong>Where it runs</strong>
              <p>Cloud HTTPS, local HTTP, or a local stdio process.</p>
            </div>
            <div className="help-fact">
              <strong>Whose account it uses</strong>
              <p>
                No account, each user’s own account, or one explicitly shared
                account.
              </p>
            </div>
            <div className="help-fact">
              <strong>Who can use it</strong>
              <p>
                Only you, the Silicons you look after, or invited Carbons and Silicons.
              </p>
            </div>
          </div>
          <div className="panel">
            <Terminal size={22} />
            <h3>Help that travels with you</h3>
            <p>Every CLI command includes its own help.</p>
            <Code>
              {
                "mcport --help\nmcport connection --help\nmcport tool call --help"
              }
            </Code>
          </div>
          {discovery && (
            <div className="resource-links">
              {[
                ["Online docs", discovery.docs_url],
                ["Source code", discovery.repository_url],
                ["Rust package", discovery.package_url],
              ]
                .filter(([, url]) => url)
                .map(([label, url]) => (
                  <a key={label} href={url} target="_blank" rel="noreferrer">
                    {label}
                    <ExternalLink size={14} />
                  </a>
                ))}
            </div>
          )}
        </aside>
      </div>
    </>
  );
}
