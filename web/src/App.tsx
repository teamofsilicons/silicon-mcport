import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  Activity as ActivityIcon,
  ArrowRight,
  Box,
  BookOpen,
  ChevronDown,
  Cloud,
  Command,
  ExternalLink,
  Globe,
  HelpCircle,
  LayoutGrid,
  Lock,
  LogOut,
  Menu,
  Monitor,
  Plus,
  Search,
  Settings2,
  ShieldCheck,
  Terminal,
  Users,
  X,
} from "lucide-react";
import {
  api,
  message,
  savedSession,
  saveSession,
  recordWebEvent,
} from "./lib/api";
import type { Connection, DirectoryEntry, Discovery, Session } from "./lib/api";
import {
  Button,
  Dialog,
  DialogContent,
  Empty,
  ErrorBox,
  Input,
  Loading,
  Logo,
  PageHead,
  Status,
} from "./ui";
import { ConnectionDetail, CreateConnection } from "./Connections";
const DirectoryPage = lazy(() =>
  import("./Directory").then((module) => ({ default: module.DirectoryPage })),
);
import { ActivityPage, HelpPage, HostsPage, SettingsPage } from "./Pages";
import { useRoute } from "./lib/routing";
import type { Page } from "./lib/routing";
const nav = [
  { id: "connections", label: "Connections", icon: LayoutGrid },
  { id: "directory", label: "Directory", icon: BookOpen },
  { id: "hosts", label: "Hosts", icon: Monitor },
  { id: "activity", label: "Activity", icon: ActivityIcon },
] as const;
export default function App() {
  const [session, setSession] = useState<Session | null>(savedSession);
  const [ready, setReady] = useState(false);
  const [discovery, setDiscovery] = useState<Discovery | null>(null);
  const [route, go] = useRoute();
  const page = route.page;
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState("");
  const [connections, setConnections] = useState<Connection[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<Connection | null>(null);
  const [create, setCreate] = useState(false);
  const [chooseSetup, setChooseSetup] = useState(false);
  const [directoryEntry, setDirectoryEntry] = useState<DirectoryEntry | null>(
    null,
  );
  const [menu, setMenu] = useState(false);
  const [toast, setToast] = useState("");
  const [logoutBusy, setLogoutBusy] = useState(false);
  const notify = useCallback((s: string) => {
    setToast(s);
  }, []);
  useEffect(() => {
    if (toast) {
      const timer = setTimeout(() => setToast(""), 4500);
      return () => clearTimeout(timer);
    }
  }, [toast]);
  useEffect(() => {
    api
      .discovery()
      .then(setDiscovery)
      .catch(() => {});
    api
      .me()
      .then((s) => {
        saveSession(s);
        setSession(s);
      })
      .catch(async (e) => {
        if (e.status === 401) {
          try {
            const s = await api.browserRefresh();
            saveSession(s);
            setSession(s);
          } catch {
            saveSession(null);
            setSession(null);
          }
        } else setError(message(e));
      })
      .finally(() => setReady(true));
  }, []);
  useEffect(() => {
    if (!session) return;
    const timer = setTimeout(
      () => {
        api
          .browserRefresh()
          .then((s) => {
            saveSession(s);
            setSession(s);
          })
          .catch((e) => {
            setError(message(e));
          });
      },
      Math.max(1000, session.expires_at * 1000 - Date.now() - 60000),
    );
    return () => clearTimeout(timer);
  }, [session]);
  const reload = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setConnections(await api.connections());
    } catch (e) {
      setError(message(e));
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => {
    if (session && ready) void reload();
  }, [session, ready, reload]);
  useEffect(() => {
    if (!session || !ready || !route.connectionId || page !== "connections")
      return;
    let active = true;
    setDetailLoading(true);
    setDetailError("");
    api
      .connection(route.connectionId)
      .then((c) => {
        if (active) setSelected(c);
      })
      .catch((e) => {
        if (active) setDetailError(message(e));
      })
      .finally(() => {
        if (active) setDetailLoading(false);
      });
    return () => {
      active = false;
    };
  }, [
    route.connectionId,
    page,
    session?.actor.principal_id,
    session?.environment,
    ready,
  ]);
  const selectConnection = (c: Connection) => {
    setSelected(c);
    go({ page: "connections", connectionId: c.id, tab: "tools" });
  };
  const navigate = (next: Page) => {
    go({ page: next });
    recordWebEvent("navigation", "render", "success");
    setSelected(null);
    setMenu(false);
  };
  const startConnection = (entry: DirectoryEntry | null = null) => {
    setDirectoryEntry(entry);
    setChooseSetup(false);
    setCreate(true);
  };
  const login = (s: Session) => {
    saveSession(s);
    setSession(s);
    setReady(true);
    void api.settings().catch(() => {});
    recordWebEvent("login", "complete", "success");
    notify(`Welcome, ${s.actor.display_name || s.actor.principal_id}.`);
  };
  const logout = async () => {
    setLogoutBusy(true);
    try {
      await api.logout();
      saveSession(null);
      setSession(null);
      setConnections([]);
      setSelected(null);
    } catch (e) {
      notify(message(e));
    } finally {
      setLogoutBusy(false);
    }
  };
  if (!ready)
    return (
      <div className="initial-loading">
        <Logo />
        <Loading />
      </div>
    );
  if (!session) return <Welcome discovery={discovery} onLogin={login} />;
  return (
    <div className="app-shell">
      <a className="skip-link" href="#main">
        Skip to content
      </a>
      <aside className={`sidebar ${menu ? "is-open" : ""}`}>
        <div className="sidebar-brand">
          <Logo />
          <button
            className="icon-button mobile-close"
            aria-label="Close navigation"
            onClick={() => setMenu(false)}
          >
            <X size={20} />
          </button>
        </div>
        <div className="workspace-label">
          <span className="workspace-avatar">
            {session.actor.org_id.slice(0, 1).toUpperCase()}
          </span>
          <span>
            <strong>{session.actor.org_id}</strong>
            <small>
              {session.environment === "production"
                ? "Organization workspace"
                : `Test · ${session.environment}`}
            </small>
          </span>
        </div>
        <div className="nav-caption">WORKSPACE</div>
        <nav aria-label="Main navigation">
          {nav.map((n) => (
            <button
              key={n.id}
              className={`nav-item ${page === n.id ? "active" : ""}`}
              onClick={() => navigate(n.id)}
              aria-current={page === n.id ? "page" : undefined}
            >
              <n.icon size={18} />
              <span>{n.label}</span>
              {n.id === "connections" && connections.length > 0 && (
                <span className="nav-count">{connections.length}</span>
              )}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="cli-promo">
            <Terminal size={20} />
            <strong>Built for your terminal, too.</strong>
            <p>Same connections. Same permissions. Anywhere.</p>
            <button onClick={() => navigate("help")}>
              Get the CLI <ArrowRight size={14} />
            </button>
          </div>
          <button
            className={`nav-item ${page === "settings" ? "active" : ""}`}
            onClick={() => navigate("settings")}
          >
            <Settings2 size={18} />
            <span>Settings</span>
          </button>
          <button
            className={`nav-item ${page === "help" ? "active" : ""}`}
            onClick={() => navigate("help")}
          >
            <HelpCircle size={18} />
            <span>Help & documentation</span>
            <ExternalLink size={13} />
          </button>
          <div className="profile">
            <span className="profile-avatar">
              {session.actor.identity_kind === "silicon" ? (
                <Command size={18} />
              ) : (
                session.actor.display_name.slice(0, 1) || "C"
              )}
            </span>
            <span>
              <strong>
                {session.actor.display_name || session.actor.principal_id}
              </strong>
              <small>
                {session.actor.identity_kind === "silicon"
                  ? "Silicon"
                  : "Carbon"}{" "}
                account
              </small>
            </span>
            <button
              disabled={logoutBusy}
              className="icon-button"
              onClick={() => void logout()}
              aria-label="Sign out"
            >
              <LogOut size={16} />
            </button>
          </div>
        </div>
      </aside>
      {menu && (
        <button
          className="nav-scrim"
          aria-label="Close navigation"
          onClick={() => setMenu(false)}
        />
      )}
      <div className="main-wrap">
        <header className="topbar">
          <div>
            <button
              className="icon-button mobile-menu"
              aria-label="Open navigation"
              onClick={() => setMenu(true)}
            >
              <Menu size={20} />
            </button>
            <span className="topbar-label">Workspace</span>
            <span className="slash">/</span>
            <strong>
              {page === "connections" &&
              route.connectionId &&
              selected?.id === route.connectionId
                ? selected.name
                : page === "help"
                  ? "Getting started"
                  : page[0].toUpperCase() + page.slice(1)}
            </strong>
          </div>
          <a
            className="docs-link"
            href="https://docs.honeycomb.teamofsilicons.com/guides/team-of-silicons-ready-applications/"
            target="_blank"
            rel="noreferrer"
          >
            <span className="silicon-dot" />
            Team of Silicons
            <ExternalLink size={13} />
          </a>
        </header>
        <main id="main" tabIndex={-1}>
          {page === "connections" &&
            (route.connectionId ? (
              detailError ? (
                <>
                  <ErrorBox error={detailError} />
                  <Button
                    variant="secondary"
                    onClick={() => navigate("connections")}
                  >
                    Back to connections
                  </Button>
                </>
              ) : detailLoading || selected?.id !== route.connectionId ? (
                <Loading />
              ) : (
                <ConnectionDetail
                  key={selected.id}
                  connection={selected}
                  tab={route.tab || "tools"}
                  onTabChange={(tab) => go({ ...route, tab })}
                  onBack={() => {
                    navigate("connections");
                    void reload();
                  }}
                  onUpdate={(c) => {
                    setSelected(c);
                    void reload();
                  }}
                  notify={notify}
                />
              )
            ) : (
              <Catalog
                connections={connections}
                session={session}
                loading={loading}
                error={error}
                onRefresh={reload}
                onCreate={() => setChooseSetup(true)}
                onSelect={selectConnection}
                onHelp={() => navigate("help")}
              />
            ))}
          {page === "directory" && (
            <Suspense fallback={<Loading />}>
              <DirectoryPage
                onUse={startConnection}
                onCustom={() => startConnection()}
                notify={notify}
              />
            </Suspense>
          )}
          {page === "hosts" && <HostsPage />}
          {page === "activity" && (
            <ActivityPage
              callId={route.callId}
              onSelect={(callId) => go({ page: "activity", callId })}
            />
          )}
          {page === "settings" && (
            <SettingsPage
              session={session}
              discovery={discovery}
              notify={notify}
            />
          )}{" "}
          {page === "help" && <HelpPage discovery={discovery} />}
        </main>
        <footer className="app-footer">
          <span>Silicon MCPort</span>
          <span>Your tools. Your people. Connected.</span>
        </footer>
      </div>
      <Dialog open={chooseSetup} onOpenChange={setChooseSetup}>
        <DialogContent
          className="wide-dialog"
          title="Add a connection"
          description="Start with a directory entry or configure your own MCP endpoint."
        >
          <div className="choice-grid setup-choices">
            <button
              className="choice"
              onClick={() => {
                setChooseSetup(false);
                navigate("directory");
              }}
            >
              <BookOpen size={25} />
              <strong>Browse the directory</strong>
              <span>
                Community MCPs from mcpservers.org and your organization’s
                templates.
              </span>
              <span className="setup-choice-link">
                Find an MCP <ArrowRight size={15} />
              </span>
            </button>
            <button className="choice" onClick={() => startConnection()}>
              <Globe size={25} />
              <strong>Custom endpoint</strong>
              <span>
                Set up a cloud endpoint, local HTTP server or a stdio process.
              </span>
              <span className="setup-choice-link">
                Configure manually <ArrowRight size={15} />
              </span>
            </button>
          </div>
        </DialogContent>
      </Dialog>
      <CreateConnection
        open={create}
        entry={directoryEntry}
        onClose={() => setCreate(false)}
        onCreated={(c) => {
          setCreate(false);
          selectConnection(c);
          void reload();
          recordWebEvent("connection.create", "complete", "success");
          notify("Connection created. Discover its tools to get started.");
        }}
      />
      {toast && (
        <div className="toast" role="status">
          <ShieldCheck size={17} />
          <span>{toast}</span>
          <button
            aria-label="Dismiss notification"
            className="icon-button"
            onClick={() => setToast("")}
          >
            <X size={15} />
          </button>
        </div>
      )}
    </div>
  );
}
function Welcome({
  discovery,
  onLogin,
}: {
  discovery: Discovery | null;
  onLogin: (s: Session) => void;
}) {
  const [kind, setKind] = useState<"carbon" | "silicon" | null>(null);
  const [slt, setSlt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [manual, setManual] = useState(false);
  const [popupUrl, setPopupUrl] = useState("");
  const [waiting, setWaiting] = useState(false);
  const popup = useRef<Window | null>(null);
  const attempt = useRef<{ state: string; origin: string } | null>(null);
  useEffect(() => {
    const listener = (event: MessageEvent) => {
      const a = attempt.current;
      if (
        !a ||
        event.source !== popup.current ||
        event.origin !== window.location.origin
      )
        return;
      const data = event.data;
      if (
        !data ||
        data.state !== a.state ||
        data.type !== "mcport-auth-complete"
      )
        return;
      setBusy(true);
      setWaiting(false);
      setError("");
      api
        .me()
        .then((s) => {
          if (s.actor.identity_kind !== kind)
            throw new Error(
              "IAM returned a different identity type. Choose the matching login.",
            );
          popup.current?.close();
          onLogin(s);
        })
        .catch((e) => setError(message(e)))
        .finally(() => setBusy(false));
    };
    window.addEventListener("message", listener);
    return () => window.removeEventListener("message", listener);
  }, [kind, onLogin]);
  useEffect(() => {
    if (!waiting) return;
    const timer = setInterval(() => {
      if (popup.current?.closed) {
        setWaiting(false);
        setError(
          "The sign-in window closed. Open it again, continue in this tab, or paste an app-bound token.",
        );
      }
    }, 750);
    return () => clearInterval(timer);
  }, [waiting]);
  useEffect(() => {
    setWaiting(false);
    setPopupUrl("");
  }, [kind]);
  async function browserLogin() {
    if (!kind) return;
    setError("");
    setBusy(true);
    popup.current = window.open(
      "about:blank",
      "mcport-iam-login",
      "popup,width=520,height=720",
    );
    if (!popup.current) {
      setError(
        "The sign-in window was blocked. Allow popups, or use an app-bound token below.",
      );
    }
    try {
      const a = await api.browserStart(kind);
      attempt.current = { state: a.state, origin: window.location.origin };
      setPopupUrl(a.url);
      if (popup.current) {
        popup.current.location.href = a.url;
        setWaiting(true);
      }
    } catch (e) {
      popup.current?.close();
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  async function submit() {
    if (!kind) return;
    setBusy(true);
    setError("");
    try {
      const s = await api.login(slt.trim(), kind);
      if (s.actor.identity_kind !== kind)
        throw new Error(
          "IAM returned a different identity type. Choose the matching login.",
        );
      setSlt("");
      onLogin(s);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="welcome">
      <header>
        <Logo />
        <a
          href="https://docs.honeycomb.teamofsilicons.com/guides/team-of-silicons-ready-applications/"
          target="_blank"
          rel="noreferrer"
        >
          Part of the Silicon ecosystem <ArrowRight size={15} />
        </a>
      </header>
      <main className="welcome-main">
        <div className="welcome-copy">
          <div className="eyebrow">
            <span className="silicon-dot" />
            ONE WORKSPACE. EVERY MCP.
          </div>
          <h1>
            Your tools,
            <br />
            connected.
          </h1>
          <p>
            Bring your cloud and local MCPs together. Share them with the right
            people and Silicons. Use them from anywhere.
          </p>
          <div className="welcome-actions">
            <Button
              size="lg"
              onClick={() => {
                setKind("carbon");
                setError("");
              }}
            >
              Continue as Carbon <ArrowRight size={17} />
            </Button>
            <Button
              size="lg"
              variant="secondary"
              onClick={() => {
                setKind("silicon");
                setError("");
              }}
            >
              <Command size={17} />
              Continue as Silicon
            </Button>
          </div>
          <div className="welcome-trust">
            <ShieldCheck size={16} />
            Your accounts stay yours. Access stays in your control.
          </div>
        </div>
        <div
          className="welcome-art"
          aria-label="Cloud and local MCPs connect through MCPort to people and Silicons"
        >
          <div className="art-grid" />
          <div className="art-tag tag-cloud">
            <Cloud size={18} />
            <span>
              Cloud MCPs<small>Your favorite tools</small>
            </span>
          </div>
          <div className="art-tag tag-local">
            <Monitor size={18} />
            <span>
              Local MCPs<small>On your machine</small>
            </span>
          </div>
          <div className="art-center">
            <img src="/favicon.svg" alt="" />
            <strong>MCPort</strong>
            <small>One place to connect</small>
          </div>
          <svg className="art-lines" viewBox="0 0 480 440" aria-hidden="true">
            <path
              d="M110 90V160Q110 220 210 220M370 90V160Q370 220 270 220M240 255V335M130 360H350"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeDasharray="5 5"
            />
          </svg>
          <div className="art-tag tag-people">
            <Users size={18} />
            <span>
              Carbons & Silicons<small>Only the access you give</small>
            </span>
          </div>
          <span className="art-caption">
            <span className="silicon-dot" />
            LOCAL OR CLOUD. ALWAYS YOURS.
          </span>
        </div>
      </main>
      <section className="welcome-features">
        <article>
          <Globe size={20} />
          <h3>Configure once</h3>
          <p>
            HTTP or stdio. Personal accounts, shared accounts, or no
            authentication.
          </p>
        </article>
        <article>
          <Users size={20} />
          <h3>Share intentionally</h3>
          <p>Open it to your organization or invite specific people.</p>
        </article>
        <article>
          <Terminal size={20} />
          <h3>Work from anywhere</h3>
          <p>
            One consistent CLI for your tools, even when they live on another
            machine.
          </p>
        </article>
      </section>
      <footer>
        <span>© {new Date().getFullYear()} Silicon MCPort</span>
        <a href="https://uiarc.dev" target="_blank" rel="noreferrer">
          Crafted with Arc UI
        </a>
      </footer>
      <Dialog
        open={kind !== null}
        onOpenChange={(v) => {
          if (!v) {
            setKind(null);
            setSlt("");
            setError("");
          }
        }}
      >
        <DialogContent
          title={`Sign in as a ${kind === "silicon" ? "Silicon" : "Carbon"}`}
          description="Your identity is verified by Silicon IAM. Provider accounts are connected separately."
        >
          <div className="form-stack">
            {error && <ErrorBox error={error} />}
            <Button loading={busy} onClick={() => void browserLogin()}>
              {waiting ? "Reopen Silicon IAM" : "Open Silicon IAM"}{" "}
              <ExternalLink size={16} />
            </Button>
            {popupUrl && (
              <div className="notice-block" role="status">
                <strong>
                  {waiting
                    ? "Waiting for you in the IAM sign-in window"
                    : "Continue your IAM sign-in"}
                </strong>
                <p>
                  If the window did not appear, finish signing in here. You will
                  return to MCPort afterward.
                </p>
                <a
                  className="text-button"
                  href={popupUrl}
                  onClick={() => {
                    popup.current?.close();
                    setWaiting(false);
                  }}
                >
                  Continue in this tab <ArrowRight size={14} />
                </a>
              </div>
            )}
            <button className="text-button" onClick={() => setManual(!manual)}>
              {manual
                ? "Hide token login"
                : "Already have an app-bound sign-in token?"}
              <ChevronDown size={14} />
            </button>
            {manual && (
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void submit();
                }}
                className="form-stack"
              >
                <Input
                  label="App-bound SLT"
                  type="password"
                  value={slt}
                  onChange={(e) => setSlt(e.target.value)}
                  autoComplete="off"
                  placeholder="Paste your MCPort token"
                  description={`Obtain a token for ${discovery?.app_id || "MCPort"} through the official IAM CLI or website.`}
                />
                <Button type="submit" loading={busy} disabled={!slt.trim()}>
                  Sign in to MCPort <ArrowRight size={16} />
                </Button>
              </form>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
function Catalog({
  connections,
  session,
  loading,
  error,
  onRefresh,
  onCreate,
  onSelect,
  onHelp,
}: {
  connections: Connection[];
  session: Session;
  loading: boolean;
  error: string;
  onRefresh: () => void;
  onCreate: () => void;
  onSelect: (c: Connection) => void;
  onHelp: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const filtered = connections.filter(
    (c) =>
      (filter === "all" ||
        (filter === "mine" && c.owner_id === session.actor.principal_id) ||
        (filter === "shared" && c.owner_id !== session.actor.principal_id) ||
        (filter === "local" && !!c.host_id)) &&
      `${c.name} ${c.description} ${c.url || ""}`
        .toLowerCase()
        .includes(query.toLowerCase()),
  );
  return (
    <>
      <PageHead
        eyebrow="YOUR WORKSPACE"
        title="Connections"
        description="All your MCPs, with the right access. Ready wherever you work."
        action={
          <Button onClick={onCreate}>
            <Plus size={17} />
            New connection
          </Button>
        }
      />
      <div className="overview-strip">
        <div>
          <span className="metric-icon mint">
            <Box size={19} />
          </span>
          <span>
            <strong>{connections.length}</strong>
            <small>Total connections</small>
          </span>
        </div>
        <div>
          <span className="metric-icon blue">
            <Globe size={19} />
          </span>
          <span>
            <strong>{connections.filter((c) => !c.host_id).length}</strong>
            <small>Cloud connections</small>
          </span>
        </div>
        <div>
          <span className="metric-icon peach">
            <Monitor size={19} />
          </span>
          <span>
            <strong>{connections.filter((c) => c.host_id).length}</strong>
            <small>Local connections</small>
          </span>
        </div>
        <button onClick={onHelp}>
          <Terminal size={18} />
          <span>Meet your command line</span>
          <ArrowRight size={16} />
        </button>
      </div>
      <div className="catalog-toolbar">
        <div className="filter-tabs" aria-label="Filter connections">
          {[
            ["all", "All connections"],
            ["mine", "Created by me"],
            ["shared", "Shared with me"],
            ["local", "Local"],
          ].map(([value, label]) => (
            <button
              key={value}
              className={filter === value ? "active" : ""}
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
            >
              {label}
            </button>
          ))}
        </div>
        <label className="search">
          <Search size={16} />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search connections…"
            aria-label="Search connections"
          />
          {query && (
            <button
              className="icon-button"
              onClick={() => setQuery("")}
              aria-label="Clear search"
            >
              <X size={13} />
            </button>
          )}
        </label>
      </div>
      {error ? (
        <ErrorBox error={error} onRetry={onRefresh} />
      ) : loading ? (
        <Loading />
      ) : filtered.length ? (
        <div className="connection-grid">
          {filtered.map((c) => (
            <button
              className="connection-card"
              key={c.id}
              onClick={() => onSelect(c)}
            >
              <div className="card-top">
                <span
                  className={`connection-icon ${c.host_id ? "peach" : "mint"}`}
                >
                  {c.host_id ? <Monitor size={24} /> : <Globe size={24} />}
                </span>
                <Status status={c.status} />
              </div>
              <h3>
                {c.name}
                <ArrowRight size={16} />
              </h3>
              <p>
                {c.description ||
                  "Open this connection to explore its tools and manage access."}
              </p>
              <div className="card-meta">
                <span>
                  {c.visibility === "private" ? (
                    <Lock size={13} />
                  ) : (
                    <Users size={13} />
                  )}
                  {
                    {
                      private: "Invite only",
                      org: "Organization",
                      invited: "Invite only",
                    }[c.visibility]
                  }
                </span>
                <span>
                  {c.host_id ? "Local host" : "Cloud"} ·{" "}
                  {c.transport === "stdio" ? "stdio" : "HTTP"}
                </span>
              </div>
              <div className="card-account">
                <span className="mini-avatar">
                  {c.auth_mode === "shared" ? (
                    <Users size={12} />
                  ) : (
                    <ShieldCheck size={12} />
                  )}
                </span>
                <span>
                  {c.auth_mode === "none"
                    ? "No provider account needed"
                    : c.account?.connected
                      ? `Using ${c.account.label || c.account.owner_id}`
                      : c.auth_mode === "per-user"
                        ? "Uses your own account"
                        : "Shared account setup needed"}
                </span>
              </div>
            </button>
          ))}
          <button className="connection-card add-card" onClick={onCreate}>
            <span className="add-circle">
              <Plus size={24} />
            </span>
            <strong>Connect another MCP</strong>
            <span>
              Cloud tools or a local server.
              <br />
              Bring them all together.
            </span>
          </button>
        </div>
      ) : (
        <Empty
          title={
            query || filter !== "all"
              ? "No matching connections"
              : "A home for your MCPs"
          }
          description={
            query || filter !== "all"
              ? "Try another search or filter to find your connection."
              : "Connect a cloud tool or something running on your machine. You decide who can use it."
          }
          icon={<Box size={26} />}
          action={
            query || filter !== "all" ? (
              <Button
                variant="secondary"
                onClick={() => {
                  setQuery("");
                  setFilter("all");
                }}
              >
                Clear filters
              </Button>
            ) : (
              <Button onClick={onCreate}>
                <Plus size={17} />
                Create your first connection
              </Button>
            )
          }
        />
      )}
      <div className="workspace-note">
        <ShieldCheck size={15} />
        <span>
          Only connections you have access to appear here. Your credentials are
          never shared with other callers.
        </span>
      </div>
    </>
  );
}

export function AuthCallback({
  slt,
  state,
}: {
  slt: string | null;
  state: string | null;
}) {
  const started = useRef(false);
  const [error, setError] = useState("");
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    if (!slt || !state) {
      setError(
        "The sign-in callback is missing its token or state. Close this window and start sign-in again.",
      );
      return;
    }
    api
      .browserComplete(slt, state)
      .then((s) => {
        saveSession(s);
        setDone(true);
        if (window.opener) {
          window.opener.postMessage(
            { type: "mcport-auth-complete", state },
            window.location.origin,
          );
          window.close();
        }
      })
      .catch((e) => setError(message(e)));
  }, [slt, state]);
  return (
    <div className="callback-page">
      <Logo />
      {error ? (
        <ErrorBox error={error} />
      ) : done ? (
        <>
          <h2>You're signed in.</h2>
          <p>You can return to your MCPort workspace.</p>
          <a href="/">
            Open workspace <ArrowRight size={15} />
          </a>
        </>
      ) : (
        <>
          <Loading />
          <p>Verifying your identity with Silicon IAM…</p>
        </>
      )}
    </div>
  );
}
