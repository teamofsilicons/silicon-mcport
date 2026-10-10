"use client";
import { ProviderCustody } from "./sharing";
import type { ConnectionTab } from "./lib/routing";
import { useEffect, useMemo, useState } from "react";
import {
  ArrowLeft,
  ArrowRight,
  Check,
  Cloud,
  Code2,
  ExternalLink,
  FileText,
  Globe,
  KeyRound,
  Lock,
  Monitor,
  Play,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  Trash2,
  Users,
  Wrench,
} from "lucide-react";
import { api, message } from "./lib/api";
import {
  changeAuthentication,
  changeTransport,
  connectionFromDirectory,
  isAbsoluteCommand,
  sourceLink,
} from "./lib/directory";
import { McpResult } from "./McpResult";
import type {
  Access,
  Account,
  CallResult,
  Connection,
  ConnectionInput,
  DirectoryEntry,
  Host,
  Policy,
  Tool,
} from "./lib/api";
import {
  Breadcrumb,
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
  Select,
  Status,
  Switch,
  Textarea,
} from "./ui";
const pretty = (v: unknown) => JSON.stringify(v, null, 2);
export function CreateConnection({
  open,
  onClose,
  onCreated,
  entry,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: (c: Connection) => void;
  entry?: DirectoryEntry | null;
}) {
  const [step, setStep] = useState(0);
  const [location, setLocation] = useState<"cloud" | "local">("cloud");
  const [data, setData] = useState<ConnectionInput>(() =>
    connectionFromDirectory(),
  );
  const [visibilityChosen, setVisibilityChosen] = useState(false);
  const [args, setArgs] = useState("");
  const [hosts, setHosts] = useState<Host[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const patch = (v: Partial<ConnectionInput>) =>
    setData((d) => ({ ...d, ...v }));
  useEffect(() => {
    if (open) {
      setStep(0);
      setError("");
      const initial = connectionFromDirectory(entry);
      setData(initial);
      setVisibilityChosen(false);
      setLocation(
        initial.transport === "stdio" || initial.url?.startsWith("http:")
          ? "local"
          : "cloud",
      );
      setArgs(initial.args.join("\n"));
      setHosts([]);
      api
        .hosts()
        .then(setHosts)
        .catch((e) => setError(message(e)));
    }
  }, [open, entry]);
  function next() {
    setError("");
    if (step === 0 && !/^[a-zA-Z0-9_-]{1,80}$/.test(data.name.trim())) {
      setError(
        "Use 1–80 letters, numbers, hyphens or underscores for the connection name.",
      );
      return;
    }
    if (step === 1) {
      if (location === "local" && !data.host_id) {
        setError(
          "Choose a registered host. Register this machine with the CLI first.",
        );
        return;
      }
      if (data.transport === "http") {
        try {
          const url = new URL(data.url || "");
          if (!["https:", "http:"].includes(url.protocol)) throw Error();
          const loopbackDev = ["localhost", "127.0.0.1", "[::1]"].includes(window.location.hostname) && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
          if (location === "cloud" && url.protocol !== "https:" && !loopbackDev) {
            setError(
              "Cloud MCPs require an HTTPS endpoint. Use a local host for localhost HTTP.",
            );
            return;
          }
        } catch {
          setError("Enter a complete HTTP or HTTPS MCP endpoint.");
          return;
        }
      } else if (!isAbsoluteCommand(data.command?.trim() || "")) {
        setError("Enter the absolute path to the MCP executable on your host.");
        return;
      }
    }
    setStep(step + 1);
  }
  async function submit() {
    setBusy(true);
    setError("");
    try {
      onCreated(
        await api.createConnection({
          ...data,
          name: data.name.trim(),
          host_id: location === "local" ? data.host_id : undefined,
          url: data.transport === "http" ? data.url : undefined,
          command: data.transport === "stdio" ? data.command : undefined,
          args:
            data.transport === "stdio" ? args.split("\n").filter(Boolean) : [],
        }),
      );
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (!v && !busy) onClose();
      }}
    >
      <DialogContent
        className="wide-dialog"
        title="New connection"
        description="Make an MCP available in your workspace. You can change access later."
      >
        {entry && (
          <div className="directory-selection">
            <strong>From the directory: {entry.name}</strong>
            <span>
              {entry.source === "community"
                ? "Community entry · mcpservers.org"
                : "Organization entry"}
            </span>
            {sourceLink(entry.source_url) && (
              <a
                href={sourceLink(entry.source_url)}
                target="_blank"
                rel="noreferrer"
              >
                {entry.source_url} <ExternalLink size={13} />
              </a>
            )}
            <p>
              Review the endpoint and setup below before creating a connection.
              Selecting a template does not run an MCP tool.
            </p>
          </div>
        )}
        <div className="wizard-steps">
          {["Basics", "Endpoint", "Account & access"].map((s, i) => (
            <span
              className={step === i ? "current" : step > i ? "done" : ""}
              key={s}
            >
              <b>{step > i ? <Check size={12} /> : i + 1}</b>
              {s}
            </span>
          ))}
        </div>
        <form
          className="form-stack"
          onSubmit={(e) => {
            e.preventDefault();
            if (step === 2) void submit(); else next();
          }}
        >
          {error && <ErrorBox error={error} />}{" "}
          {step === 0 && (
            <>
              <Input
                autoFocus
                label="Connection name"
                value={data.name}
                onChange={(e) => patch({ name: e.target.value })}
                placeholder="e.g. design-workspace"
                description="A short, memorable name you can also use in the CLI."
                maxLength={80}
              />
              <Textarea
                label="Description (optional)"
                value={data.description}
                onChange={(v) => patch({ description: v })}
                rows={2}
                placeholder="What does this MCP help your team do?"
              />
              <div className="field-title">Where does it run?</div>
              <div className="choice-grid">
                <button
                  type="button"
                  className={`choice ${location === "cloud" ? "selected" : ""}`}
                  onClick={() => {
                    setLocation("cloud");
                    setData((d) => ({
                      ...changeTransport(d, "http"),
                      host_id: undefined,
                    }));
                    setArgs("");
                  }}
                >
                  <Cloud size={23} />
                  <strong>In the cloud</strong>
                  <span>A remote HTTPS MCP endpoint.</span>
                </button>
                <button
                  type="button"
                  className={`choice ${location === "local" ? "selected" : ""}`}
                  onClick={() => setLocation("local")}
                >
                  <Monitor size={23} />
                  <strong>On a local machine</strong>
                  <span>Desktop HTTP or a stdio process.</span>
                </button>
              </div>
            </>
          )}
          {step === 1 && (
            <>
              {location === "local" && (
                <>
                  {hosts.length ? (
                    <Select
                      label="Execution host"
                      value={data.host_id || ""}
                      onChange={(v) => patch({ host_id: v })}
                      description="Requests run on this machine, even when called remotely."
                    >
                      <option value="">Choose a host</option>
                      {hosts.map((h) => (
                        <option key={h.id} value={h.id}>
                          {h.name} · {h.online ? "online" : "offline"}
                        </option>
                      ))}
                    </Select>
                  ) : (
                    <div className="notice-block">
                      <Monitor size={20} />
                      <strong>Register a host first</strong>
                      <p>
                        Run this on the machine where the MCP lives, then reopen
                        this dialog.
                      </p>
                      <Code>{"mcport host new my-mac"}</Code>
                    </div>
                  )}
                  <Select
                    label="Transport"
                    value={data.transport}
                    onChange={(v) => {
                      setData((d) => changeTransport(d, v as "http" | "stdio"));
                      if (v === "http") setArgs("");
                    }}
                  >
                    <option value="http">Local HTTP</option>
                    <option value="stdio">stdio process</option>
                  </Select>
                </>
              )}
              {data.transport === "http" ? (
                <Input
                  label="MCP endpoint URL"
                  value={data.url || ""}
                  onChange={(e) => patch({ url: e.target.value })}
                  placeholder={
                    location === "cloud"
                      ? "https://mcp.example.com/mcp"
                      : "http://127.0.0.1:3845/mcp"
                  }
                  description="Use the MCP endpoint, not the provider's website."
                />
              ) : (
                <>
                  <Input
                    label="Command"
                    value={data.command || ""}
                    onChange={(e) => patch({ command: e.target.value })}
                    placeholder="/absolute/path/to/mcp-server"
                  />
                  <Textarea
                    label="Arguments (one per line)"
                    value={args}
                    onChange={setArgs}
                    code
                    rows={3}
                    placeholder="/path/to/allowed-folder"
                  />
                </>
              )}
              {location === "local" && (
                <Notice>
                  The daemon invokes only endpoints registered on its host.
                  Credentials stay on that machine.
                </Notice>
              )}
            </>
          )}
          {step === 2 && (
            <>
              <Select
                label="Provider authentication"
                value={data.auth_mode}
                onChange={(v) =>
                  setData((d) =>
                    changeAuthentication(
                      d,
                      v as Connection["auth_mode"],
                      visibilityChosen,
                    ),
                  )
                }
              >
                <option value="none">No authentication</option>
                <option value="per-user">
                  Each user connects their own account
                </option>
                <option value="shared">Use one shared account</option>
              </Select>
              <p className="field-help">
                {data.auth_mode === "none"
                  ? "People with access can use this MCP without an upstream account."
                  : data.auth_mode === "per-user"
                    ? "Each Carbon and Silicon connects their own provider account. Accounts never fall back to someone else’s."
                    : "You authorize an account once. People you grant access can act through it without receiving its credentials."}
              </p>
              <Select
                label="Who can use this connection?"
                value={data.visibility}
                onChange={(v) => {
                  setVisibilityChosen(true);
                  patch({ visibility: v as Connection["visibility"] });
                }}
              >
                <option value="invited">Invite only</option>
                <option value="circle">Me and my Silicons</option>
              </Select>
              <p className="field-help">
                {data.visibility === "circle"
                  ? "The owner’s custodial group can use this connection, subject to its tool and provider account permissions."
                  : "Only you can use this connection until you invite people. Directory membership does not grant connection access."}
              </p>
              <div className="review-summary">
                <span>
                  {location === "cloud" ? (
                    <Cloud size={18} />
                  ) : (
                    <Monitor size={18} />
                  )}
                </span>
                <div>
                  <strong>{data.name}</strong>
                  <small>
                    {data.transport === "http" ? data.url : data.command}
                  </small>
                </div>
                <span className="small-pill">
                  {data.transport.toUpperCase()}
                </span>
              </div>
              <Notice>
                All tools start enabled. You can turn them off for everyone or
                for a specific person.
              </Notice>
            </>
          )}
          <div className="form-actions">
            <Button
              type="button"
              variant="ghost"
              onClick={() => (step ? setStep(step - 1) : onClose())}
              disabled={busy}
            >
              {step ? (
                <>
                  <ArrowLeft size={15} />
                  Back
                </>
              ) : (
                "Cancel"
              )}
            </Button>
            <Button type="submit" loading={busy}>
              {step === 2 ? (
                <>
                  <Plus size={16} />
                  Create connection
                </>
              ) : (
                <>
                  Continue
                  <ArrowRight size={16} />
                </>
              )}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
export function ConnectionDetail({
  connection: c,
  tab,
  onTabChange,
  onBack,
  onUpdate,
  notify,
}: {
  connection: Connection;
  tab: ConnectionTab;
  onTabChange: (tab: ConnectionTab) => void;
  onBack: () => void;
  onUpdate: (c: Connection) => void;
  notify: (s: string) => void;
}) {
  return (
    <>
      <Breadcrumb name={c.name} onBack={onBack} />
      <PageHead
        title={c.name}
        description={
          c.description || "Configure, share, and use this MCP connection."
        }
        action={<Status status={c.status} />}
      />
      <div className="connection-summary">
        <span>
          {c.host_id ? <Monitor size={15} /> : <Globe size={15} />}{" "}
          {c.host_id ? "Local host" : "Cloud endpoint"}
        </span>
        <span>
          {c.visibility === "invited" ? (
            <Lock size={15} />
          ) : (
            <Users size={15} />
          )}
          {
            {
              circle: "You and your Silicons",
              invited: "Invite only",
            }[c.visibility]
          }
        </span>
        <span>
          <KeyRound size={15} />
          {c.auth_mode === "none"
            ? "No provider account"
            : c.account?.connected
              ? `Using ${c.account.label || (c.account.account?.id || c.account.account?.uuid || "provider account")}`
              : c.auth_mode === "shared"
                ? "Shared provider account"
                : "Your personal account"}
        </span>
        <span className="small-pill">{c.transport.toUpperCase()}</span>
      </div>
      <div
        className="detail-tabs"
        role="tablist"
        aria-label="Connection sections"
      >
        {[
          { id: "tools", label: "Tools", icon: Wrench },
          { id: "account", label: "Account", icon: KeyRound },
          { id: "access", label: "Access", icon: Users },
          { id: "resources", label: "Resources & prompts", icon: FileText },
          { id: "settings", label: "Configuration", icon: Settings2 },
        ].map((t) => (
          <button
            role="tab"
            aria-selected={tab === t.id}
            key={t.id}
            onClick={() => onTabChange(t.id as ConnectionTab)}
            className={tab === t.id ? "active" : ""}
          >
            <t.icon size={16} />
            {t.label}
          </button>
        ))}
      </div>
      {c.host_id && c.can_manage && (
        <details
          className="local-setup"
          open={c.status === "offline" || c.status === "unregistered"}
        >
          <summary>
            <Monitor size={16} />
            Local host setup · approve this connection once on its host
          </summary>
          <p>
            Run this as the connection owner on the registered machine. This
            explicitly adds the endpoint or process to its local allowlist.
          </p>
          <Code>{`mcport connection register ${c.id}`}</Code>
        </details>
      )}
      {tab === "tools" && <Tools connection={c} notify={notify} />}{" "}
      {tab === "account" && (
        <ProviderAccount
          connection={c}
          notify={notify}
          onAccountChange={(account) => onUpdate({ ...c, account })}
        />
      )}{" "}
      {tab === "account" && c.auth_mode === "per-user" && <ProviderCustody connection={c.id}/>}
      {tab === "access" && <ConnectionAccess connection={c} notify={notify} />}{" "}
      {tab === "resources" && <Resources connection={c} />}{" "}
      {tab === "settings" && (
        <ConnectionSettings
          connection={c}
          onUpdate={onUpdate}
          onDeleted={onBack}
          notify={notify}
        />
      )}
    </>
  );
}
function Tools({
  connection: c,
  notify,
}: {
  connection: Connection;
  notify: (s: string) => void;
}) {
  const [tools, setTools] = useState<Tool[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Tool | null>(null);
  const [input, setInput] = useState("{}");
  const [result, setResult] = useState<CallResult | null>(null);
  const [runError, setRunError] = useState("");
  const [running, setRunning] = useState(false);
  const [schema, setSchema] = useState(false);
  const [principal, setPrincipal] = useState("");
  const [policies, setPolicies] = useState<Policy[]>([]);
  const [pendingTool, setPendingTool] = useState("");
  const load = async () => {
    setLoading(true);
    setError("");
    try {
      const items = await api.tools(c.id);
      setTools(items);
      setSelected((previous) =>
        previous ? items.find((t) => t.name === previous.name) || null : null,
      );
      if (c.can_manage) setPolicies(await api.policies(c.id));
    } catch (e) {
      setError(message(e));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => {
    void load();
    // The connection id selects the external resource; button-triggered reload uses current state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [c.id]);
  async function toggle(tool: Tool, enabled: boolean) {
    setPendingTool(tool.name);
    try {
      await api.setTool(
        c.id,
        tool.name,
        enabled,
        principal.trim() || undefined,
      );
      await load();
      notify(
        `${tool.name} ${enabled ? "enabled" : "disabled"}${principal ? ` for ${principal}` : " for everyone"}.`,
      );
    } catch (e) {
      setError(message(e));
    } finally {
      setPendingTool("");
    }
  }
  async function run() {
    if (!selected) return;
    setRunError("");
    setResult(null);
    let arguments_: unknown;
    try {
      arguments_ = JSON.parse(input);
      if (
        arguments_ === null ||
        typeof arguments_ !== "object" ||
        Array.isArray(arguments_)
      )
        throw Error();
    } catch {
      setRunError(
        'Enter a valid JSON object, for example {"query":"release notes"}.',
      );
      return;
    }
    setRunning(true);
    try {
      setResult(await api.call(c.id, selected.name, arguments_));
    } catch (e) {
      setRunError(message(e));
    } finally {
      setRunning(false);
    }
  }
  const choose = (t: Tool) => {
    setSelected(t);
    setResult(null);
    setRunError("");
    setSchema(false);
    const props = (t.inputSchema.properties || {}) as Record<
      string,
      { type?: string; default?: unknown; examples?: unknown[] }
    >;
    const example: Record<string, unknown> = {};
    for (const [key, p] of Object.entries(props))
      if ((t.inputSchema.required as string[] | undefined)?.includes(key))
        example[key] =
          p.default ??
          p.examples?.[0] ??
          (p.type === "number" || p.type === "integer"
            ? 0
            : p.type === "boolean"
              ? false
              : p.type === "array"
                ? []
                : p.type === "object"
                  ? {}
                  : "");
    setInput(pretty(example));
  };
  const resultUrl = useMemo(
    () =>
      result
        ? URL.createObjectURL(
            new Blob([pretty(result.result)], { type: "application/json" }),
          )
        : null,
    [result],
  );
  useEffect(
    () => () => {
      if (resultUrl) URL.revokeObjectURL(resultUrl);
    },
    [resultUrl],
  );
  const filtered = tools.filter((t) =>
    `${t.name} ${t.description}`.toLowerCase().includes(query.toLowerCase()),
  );
  return (
    <>
      <div className="section-heading">
        <div>
          <h2>
            Tools <span className="count">{tools.length}</span>
          </h2>
          <p>
            Explore what this MCP can do. Inspect an input, then try a call.
          </p>
        </div>
        <Button
          variant="secondary"
          size="sm"
          loading={loading}
          onClick={() => void load()}
        >
          <RefreshCw size={14} />
          Refresh tools
        </Button>
      </div>
      {c.can_manage && (
        <div className="policy-context">
          <ShieldCheck size={17} />
          <span>Editing tool access for</span>
          <input
            aria-label="Tool permission account"
            value={principal}
            onChange={(e) => setPrincipal(e.target.value)}
            placeholder="Everyone (or enter a c: / si: ID)"
          />
          <small>Connection-wide restrictions always apply.</small>
        </div>
      )}
      {error ? (
        <ErrorBox error={error} onRetry={() => void load()} />
      ) : loading && !tools.length ? (
        <Loading />
      ) : !tools.length ? (
        <Empty
          title="No tools discovered"
          description="This MCP has not exposed tools yet. Verify the endpoint and account, then refresh."
        />
      ) : (
        <div className="tool-workspace">
          <div className="tool-list">
            <label className="search">
              <Search size={15} />
              <input
                aria-label="Search tools"
                placeholder="Search tools…"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            {filtered.length ? (
              filtered.map((t) => {
                const personal = policies.find(
                  (p) =>
                    p.tool === t.name && (p.account?.uuid === principal.trim() || p.account?.id.toLowerCase() === principal.trim().toLowerCase()),
                );
                const globallyDisabled = policies.some(
                  (p) => p.tool === t.name && !p.account && !p.enabled,
                );
                const enabled = principal.trim()
                  ? !globallyDisabled && personal?.enabled !== false
                  : t.enabled !== false;
                return (
                  <div
                    className={`tool-row ${selected?.name === t.name ? "selected" : ""}`}
                    key={t.name}
                  >
                    <button onClick={() => choose(t)}>
                      <span>
                        <Wrench size={15} />
                        <strong>{t.name}</strong>
                      </span>
                      <small>
                        {t.description ||
                          "Inspect this tool’s input schema and run it."}
                      </small>
                    </button>
                    {c.can_manage ? (
                      <Switch
                        aria-label={`${enabled ? "Disable" : "Enable"} ${t.name}${principal ? ` for ${principal}` : ""}`}
                        checked={enabled}
                        disabled={
                          pendingTool === t.name ||
                          (!!principal.trim() && globallyDisabled)
                        }
                        onCheckedChange={(v) => void toggle(t, v)}
                      />
                    ) : (
                      <span
                        className={`small-pill ${t.enabled === false ? "muted" : ""}`}
                      >
                        {t.enabled === false ? "Disabled" : "Enabled"}
                      </span>
                    )}
                  </div>
                );
              })
            ) : (
              <p className="muted-copy">No matching tools.</p>
            )}
          </div>
          <div className="tool-runner">
            {selected ? (
              <>
                <div className="runner-title">
                  <span className="metric-icon mint">
                    <Wrench size={19} />
                  </span>
                  <div>
                    <h3>{selected.name}</h3>
                    <span>Tool playground</span>
                  </div>
                </div>
                <p>{selected.description}</p>
                <div className="runner-sections">
                  <button
                    className={!schema ? "active" : ""}
                    onClick={() => setSchema(false)}
                  >
                    Input
                  </button>
                  <button
                    className={schema ? "active" : ""}
                    onClick={() => setSchema(true)}
                  >
                    JSON schema
                  </button>
                </div>
                {schema ? (
                  <pre className="json-result">
                    {pretty(selected.inputSchema)}
                  </pre>
                ) : (
                  <Textarea
                    label="Tool arguments"
                    value={input}
                    onChange={setInput}
                    code
                    rows={9}
                  />
                )}
                <div className="run-footer">
                  <span>
                    <KeyRound size={13} />
                    {c.auth_mode === "none"
                      ? "No provider account"
                      : c.account?.connected
                        ? `Using ${c.account.label || (c.account.account?.id || c.account.account?.uuid || "provider account")}`
                        : c.auth_mode === "per-user"
                          ? "Uses your connected account"
                          : "Uses the shared account"}
                  </span>
                  <Button
                    loading={running}
                    disabled={selected.enabled === false}
                    onClick={() => void run()}
                  >
                    <Play size={15} />
                    Run tool
                  </Button>
                </div>
                {running && (
                  <div role="status" className="notice">
                    Request in progress. Activity shows live execution status
                    and cancellation.
                  </div>
                )}
                {runError && <ErrorBox error={runError} />}{" "}
                {result && (
                  <div className="result-panel">
                    <div>
                      <Status
                        status={result.result.isError ? "error" : "completed"}
                      />
                      <small>Call {result.call_id}</small>
                    </div>
                    <McpResult result={result.result} callId={result.call_id} />
                    <a
                      href={resultUrl || undefined}
                      download={`${selected.name}-result.json`}
                      className="text-button"
                    >
                      Download result JSON <ArrowRight size={13} />
                    </a>
                  </div>
                )}
                <Code>{`mcport tool call ${c.name} ${selected.name} --input '${input.replaceAll("'", "'\\''")}' --json`}</Code>
              </>
            ) : (
              <Empty
                title="Choose a tool"
                description="See its inputs, review the schema, and call it with your connection’s permissions."
                icon={<Code2 size={24} />}
              />
            )}
          </div>
        </div>
      )}
    </>
  );
}
function ProviderAccount({
  connection: c,
  notify,
  onAccountChange,
}: {
  connection: Connection;
  notify: (s: string) => void;
  onAccountChange: (account: Account) => void;
}) {
  const [account, setAccount] = useState<Account | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [secret, setSecret] = useState("");
  const [label, setLabel] = useState("");
  const [kind, setKind] = useState("bearer");
  const [header, setHeader] = useState("");
  const [clientId, setClientId] = useState("");
  const [busy, setBusy] = useState(false);
  const canChange = c.auth_mode === "per-user" || c.can_manage;
  const load = () => {
    setLoading(true);
    api
      .account(c.id)
      .then((a) => {
        setAccount(a);
        onAccountChange(a);
        setLabel(a.label || "");
      })
      .catch((e) => setError(message(e)))
      .finally(() => setLoading(false));
  };
  // A new connection mounts a new provider editor; explicit reload uses the current callback.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, [c.id]);
  async function connect() {
    setBusy(true);
    setError("");
    try {
      const connected = await api.connectAccount(
        c.id,
        secret,
        label,
        kind,
        kind === "header" ? header : undefined,
      );
      setAccount(connected);
      onAccountChange(connected);
      setSecret("");
      notify("Provider account connected.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function authorize() {
    setBusy(true);
    setError("");
    const win = window.open(
      "about:blank",
      "mcport-provider",
      "popup,width=580,height=750",
    );
    try {
      const a = await api.authorizeAccount(c.id, clientId || undefined);
      if (win) win.location.href = a.authorization_url;
      else throw new Error("Allow popups to continue provider authorization.");
    } catch (e) {
      win?.close();
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function disconnect() {
    setBusy(true);
    setError("");
    try {
      await api.disconnectAccount(c.id);
      load();
      notify("Provider account disconnected.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="narrow-section">
      <div className="section-heading">
        <div>
          <h2>Provider account</h2>
          <p>Control whose upstream account this connection uses.</p>
        </div>
      </div>
      {error && <ErrorBox error={error} />}{" "}
      {loading ? (
        <Loading />
      ) : c.auth_mode === "none" ? (
        <Empty
          title="No provider authentication required"
          description="Your MCPort access permissions still apply to every call."
          icon={<ShieldCheck size={24} />}
        />
      ) : (
        <>
          <div className="account-card">
            <span className="metric-icon mint">
              <KeyRound size={20} />
            </span>
            <div>
              <strong>
                {c.auth_mode === "shared"
                  ? "Shared account"
                  : "Your personal account"}
              </strong>
              <p>
                {account?.connected
                  ? `Connected as ${account.label || (account.account?.id || account.account?.uuid || "provider account")}`
                  : "No provider account connected yet."}
              </p>
            </div>
            <Status
              status={account?.connected ? "connected" : "setup needed"}
            />
          </div>
          <p className="muted-copy">
            {c.auth_mode === "shared"
              ? "Everyone with connection access acts through this account. Only the owner can change it."
              : "Only your own provider account is used for your calls. Other users connect separately."}
          </p>
          {c.host_id ? (
            <div className="notice-block">
              <Monitor size={21} />
              <strong>Credentials live on the execution host</strong>
              <p>
                Connect this account on the host machine. Desktop MCPs may
                already use the desktop app’s account.
              </p>
              <Code>{`mcport account connect ${c.name}`}</Code>
            </div>
          ) : canChange ? (
            <>
              <div className="form-stack panel">
                <h3>Authorize with the provider</h3>
                <Input
                  label="OAuth client ID (if required)"
                  value={clientId}
                  onChange={(e) => setClientId(e.target.value)}
                  placeholder="Leave empty for automatic registration"
                />
                <div className="inline-actions">
                  <Button
                    variant="secondary"
                    loading={busy}
                    onClick={() => void authorize()}
                  >
                    Open provider authorization <ExternalLink size={15} />
                  </Button>
                  <Button variant="ghost" onClick={load}>
                    Check connection
                  </Button>
                </div>
              </div>
              <form
                className="form-stack panel"
                onSubmit={(e) => {
                  e.preventDefault();
                  void connect();
                }}
              >
                <h3>Or connect with a credential</h3>
                <Input
                  label="Account label"
                  value={label}
                  onChange={(e) => setLabel(e.target.value)}
                  placeholder="e.g. Design team account"
                />
                <Select label="Credential type" value={kind} onChange={setKind}>
                  <option value="bearer">Bearer token</option>
                  <option value="header">Custom HTTP header</option>
                </Select>
                {kind === "header" && (
                  <Input
                    label="Header name"
                    value={header}
                    onChange={(e) => setHeader(e.target.value)}
                    placeholder="X-API-Key"
                  />
                )}
                <Input
                  label="Secret"
                  value={secret}
                  onChange={(e) => setSecret(e.target.value)}
                  type="password"
                  autoComplete="off"
                  description="Encrypted at rest. Never included in connection exports or returned to callers."
                />
                <Button
                  type="submit"
                  loading={busy}
                  disabled={
                    !secret.trim() || (kind === "header" && !header.trim())
                  }
                >
                  Connect account <ArrowRight size={15} />
                </Button>
              </form>
              {account?.connected && (
                <Button
                  variant="danger"
                  loading={busy}
                  onClick={() => void disconnect()}
                >
                  Disconnect account
                </Button>
              )}
            </>
          ) : (
            <Notice>
              Ask the connection owner to configure the shared provider account.
            </Notice>
          )}
        </>
      )}
    </div>
  );
}
function ConnectionAccess({
  connection: c,
  notify,
}: {
  connection: Connection;
  notify: (s: string) => void;
}) {
  const [grants, setGrants] = useState<Access[]>([]);
  const [principal, setPrincipal] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const load = () => {
    setLoading(true);
    api
      .access(c.id)
      .then(setGrants)
      .catch((e) => setError(message(e)))
      .finally(() => setLoading(false));
  };
  useEffect(() => {
    if (c.can_manage) load();
    // The connection id selects the external resource; button-triggered reload uses current state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [c.id]);
  async function invite() {
    setBusy(true);
    setError("");
    try {
      await api.invite(c.id, principal.trim());
      setPrincipal("");
      load();
      notify("Access granted. Tool permissions are enabled by default.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function revoke(p: string) {
    setBusy(true);
    setError("");
    try {
      await api.revoke(c.id, p);
      load();
      notify("Access revoked. Further calls are blocked.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="narrow-section">
      <div className="section-heading">
        <div>
          <h2>People & Silicons</h2>
          <p>
            Grant permission to use this connection. The owner and a Silicon owner’s custodian manage its configuration.
          </p>
        </div>
      </div>
      <div className="account-card">
        <span className="metric-icon blue">
          <Users size={20} />
        </span>
        <div>
          <strong>
            {
              {
                  circle: "The owner’s custodial group",
                invited: "Invite only — selected people and Silicons",
              }[c.visibility]
            }
          </strong>
          <p>Change visibility in Configuration.</p>
        </div>
      </div>
      {!c.can_manage ? (
        <Notice>
          The connection owner and a Silicon owner’s custodian manage invitations and tool permissions.
        </Notice>
      ) : (
        <>
          {c.visibility === "circle" && (
            <Notice>
              A Carbon owner shares with the Silicons they look after. A Silicon owner shares with its custodian and their other Silicons. Removing an invitation alone does not remove this access.
            </Notice>
          )}
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void invite();
            }}
            className="invite-form"
          >
            <Input
              label="Carbon or Silicon ID"
              value={principal}
              onChange={(e) => setPrincipal(e.target.value)}
              placeholder="si:designer or a Carbon ID"
            />
            <Button type="submit" loading={busy} disabled={!principal.trim()}>
              <Plus size={15} />
              Grant access
            </Button>
          </form>
          {error && <ErrorBox error={error} />}
          <div className="access-list">
            <div className="access-row">
              <span className="mini-avatar">
                <ShieldCheck size={15} />
              </span>
              <div>
                <strong>{(c.owner.id || c.owner.uuid)}</strong>
                <small>Can use, configure, share, and delete</small>
              </div>
              <span className="small-pill">Owner</span>
            </div>
            {loading ? (
              <Loading />
            ) : (
              grants.map((g) => (
                <div className="access-row" key={g.account.uuid}>
                  <span className="mini-avatar">
                    <Users size={15} />
                  </span>
                  <div>
                    <strong>{g.account.id || g.account.uuid}</strong>
                    <small>Can use enabled tools</small>
                  </div>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => void revoke(g.account.uuid)}
                  >
                    Remove
                  </Button>
                </div>
              ))
            )}
          </div>
          <Notice>
            Tool access can be restricted for a specific person in the Tools
            tab. Connection-wide restrictions always apply.
          </Notice>
        </>
      )}
    </div>
  );
}
function Resources({ connection: c }: { connection: Connection }) {
  const [type, setType] = useState("resources");
  const [result, setResult] = useState<CallResult | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [target, setTarget] = useState("");
  const [args, setArgs] = useState("{}");
  async function run(read = false, cursor?: string) {
    setBusy(true);
    setError("");
    try {
      const method =
        type === "templates"
          ? "resources/templates/list"
          : type === "resources"
          ? read
            ? "resources/read"
            : "resources/list"
          : read
            ? "prompts/get"
            : "prompts/list";
      setResult(
        await api.rpc(
          c.id,
          method,
          read
            ? type === "resources"
              ? { uri: target }
              : { name: target, arguments: JSON.parse(args) }
            : cursor ? { cursor } : {},
        ),
      );
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="narrow-section">
      <div className="section-heading">
        <div>
          <h2>Resources & prompts</h2>
          <p>Explore other capabilities exposed by this MCP.</p>
        </div>
      </div>
      <div className="form-stack">
        <Select
          label="Capability"
          value={type}
          onChange={(v) => {
            setType(v);
            setResult(null);
            setError("");
          }}
        >
          <option value="resources">Resources</option>
          <option value="prompts">Prompts</option>
          <option value="templates">Resource templates</option>
        </Select>
        <Button variant="secondary" loading={busy} onClick={() => void run()}>
          List {type}
          <ArrowRight size={15} />
        </Button>
        <Input
          label={type === "resources" ? "Resource URI" : "Prompt name"}
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          placeholder={
            type === "resources"
              ? "Select a URI from the listing"
              : "Select a name from the listing"
          }
        />
        {type === "prompts" && (
          <Textarea
            label="Prompt arguments (JSON)"
            value={args}
            onChange={setArgs}
            code
            rows={3}
          />
        )}
        <Button
          loading={busy}
          disabled={!target.trim()||type==="templates"}
          onClick={() => void run(true)}
        >
          {type === "resources" ? "Read resource" : "Get prompt"}
        </Button>
        {typeof result?.result.nextCursor==="string"&&<Button variant="secondary" onClick={()=>void run(false,String(result.result.nextCursor))}>Load next page</Button>}
        {error && <ErrorBox error={error} />}{" "}
        {result && <McpResult result={result.result} callId={result.call_id} />}
      </div>
    </div>
  );
}
function ConnectionSettings({
  connection: c,
  onUpdate,
  onDeleted,
  notify,
}: {
  connection: Connection;
  onUpdate: (c: Connection) => void;
  onDeleted: () => void;
  notify: (s: string) => void;
}) {
  const [name, setName] = useState(c.name);
  const [description, setDescription] = useState(c.description);
  const [visibility, setVisibility] = useState<Connection["visibility"]>(
    c.visibility === "invited" ? "invited" : c.visibility,
  );
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  async function save() {
    setBusy(true);
    setError("");
    try {
      onUpdate(
        await api.updateConnection(c.id, {
          name: name.trim(),
          description,
          visibility,
          version: c.version,
        }),
      );
      notify("Connection updated.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function remove() {
    setBusy(true);
    setError("");
    try {
      await api.deleteConnection(c.id);
      notify("Connection deleted.");
      onDeleted();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="narrow-section">
      <div className="section-heading">
        <div>
          <h2>Connection configuration</h2>
          <p>
            Stable connection ID: <code>{c.id}</code>
          </p>
        </div>
      </div>
      {error && <ErrorBox error={error} />}
      <form
        className="form-stack"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <Input
          label="Connection name"
          value={name}
          disabled={!c.can_manage}
          onChange={(e) => setName(e.target.value)}
        />
        <Textarea
          label="Description"
          value={description}
          onChange={setDescription}
          rows={3}
        />
        {c.can_manage && (
          <Select
            label="Visibility"
            value={visibility}
            onChange={(v) => setVisibility(v as Connection["visibility"])}
          >
            <option value="invited">Invite only</option>
            <option value="circle">Me and my Silicons</option>
          </Select>
        )}
        <dl className="property-list">
          <div>
            <dt>Transport</dt>
            <dd>{c.transport}</dd>
          </div>
          <div>
            <dt>Endpoint</dt>
            <dd>
              <code>{c.url || c.command}</code>
            </dd>
          </div>
          {c.host_id && (
            <div>
              <dt>Host ID</dt>
              <dd>
                <code>{c.host_id}</code>
              </dd>
            </div>
          )}
          <div>
            <dt>Authentication</dt>
            <dd>{c.auth_mode}</dd>
          </div>

        </dl>
        {c.can_manage && (
          <Button loading={busy} type="submit" disabled={!name.trim()}>
            Save changes
          </Button>
        )}
      </form>
      <Code>{`mcport connection show ${c.name} --json`}</Code>
      {c.can_manage && (
        <div className="danger-zone">
          <div>
            <h3>Delete this connection</h3>
            <p>
              Stops new calls and removes stored grants. It does not undo
              completed actions.
            </p>
          </div>
          <Button variant="danger" size="sm" onClick={() => setConfirm(true)}>
            Delete connection
          </Button>
        </div>
      )}
      <Dialog open={confirm} onOpenChange={setConfirm}>
        <DialogContent
          title={`Delete ${c.name}?`}
          description="The connection and its access grants will be removed. Completed provider actions cannot be undone."
        >
          {error && <ErrorBox error={error} />}
          <div className="form-actions">
            <Button variant="secondary" onClick={() => setConfirm(false)}>
              Keep connection
            </Button>
            <Button
              variant="danger"
              loading={busy}
              onClick={() => void remove()}
            >
              <Trash2 size={15} />
              Delete connection
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
