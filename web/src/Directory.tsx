import { useEffect, useState } from "react";
import {
  ArrowRight,
  BookOpen,
  ExternalLink,
  Globe,
  Monitor,
  Pencil,
  Plus,
  Search,
  Trash2,
  Users,
  X,
} from "lucide-react";
import { api, message } from "./lib/api";
import type { DirectoryEntry, DirectoryInput } from "./lib/api";
import { sourceLink } from "./lib/directory";
import {
  Button,
  Dialog,
  DialogContent,
  Empty,
  ErrorBox,
  Input,
  Loading,
  Notice,
  PageHead,
  Select,
  Textarea,
} from "./ui";

export function DirectoryPage({
  onUse,
  onCustom,
  notify,
}: {
  onUse: (entry: DirectoryEntry) => void;
  onCustom: () => void;
  notify: (message: string) => void;
}) {
  const [entries, setEntries] = useState<DirectoryEntry[]>([]);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [category, setCategory] = useState("all");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const [editor, setEditor] = useState<DirectoryEntry | "new" | null>(null);
  const [deleting, setDeleting] = useState<DirectoryEntry | null>(null);
  const [deleteError, setDeleteError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    setLoading(true);
    setError("");
    const timer = setTimeout(
      () => {
        api
          .directory(query.trim())
          .then((value) => {
            if (active) setEntries(value);
          })
          .catch((e) => {
            if (active) setError(message(e));
          })
          .finally(() => {
            if (active) setLoading(false);
          });
      },
      query ? 250 : 0,
    );
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [query, revision]);
  const categories = [
    ...new Set(entries.map((e) => e.category || "Other")),
  ].sort();
  const visible = entries.filter(
    (e) =>
      (filter === "all" || e.source === filter) &&
      (category === "all" || e.category === category),
  );
  async function remove() {
    if (!deleting) return;
    setBusy(true);
    setDeleteError("");
    try {
      await api.deleteDirectoryEntry(deleting.id);
      setDeleting(null);
      setRevision((r) => r + 1);
      notify("Directory entry removed. Existing connections are unchanged.");
    } catch (e) {
      setDeleteError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <PageHead
        eyebrow="FIND YOUR NEXT CONNECTION"
        title="MCP directory"
        description="Explore community MCPs and reusable setups from your organization."
        action={
          <div className="directory-actions">
            <Button variant="secondary" onClick={onCustom}>
              Custom endpoint <ArrowRight size={15} />
            </Button>
            <Button onClick={() => setEditor("new")}>
              <Plus size={16} />
              Add org entry
            </Button>
          </div>
        }
      />
      <div className="directory-intro">
        <span className="metric-icon mint">
          <BookOpen size={23} />
        </span>
        <div>
          <h2>A starting point, with you in control.</h2>
          <p>
            Choose an entry, review its endpoint and account setup, then create
            your connection. A directory listing does not grant anyone access or
            run an MCP.
          </p>
        </div>
        <a href="https://mcpservers.org" target="_blank" rel="noreferrer">
          Community source: mcpservers.org <ExternalLink size={14} />
        </a>
      </div>
      <div className="catalog-toolbar directory-toolbar">
        <div className="filter-tabs" aria-label="Filter directory source">
          {[
            ["all", "All entries"],
            ["community", "Community"],
            ["org", "My organization"],
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
            onChange={(e) => {
              setQuery(e.target.value);
              setCategory("all");
            }}
            placeholder="Search MCPs, categories, endpoints…"
            aria-label="Search MCP directory"
          />
          {query && (
            <button
              className="icon-button"
              onClick={() => setQuery("")}
              aria-label="Clear directory search"
            >
              <X size={14} />
            </button>
          )}
        </label>
      </div>
      <div className="directory-results-head">
        <span aria-live="polite">
          {loading
            ? "Finding entries…"
            : `${visible.length} ${visible.length === 1 ? "entry" : "entries"}`}
        </span>
        <label>
          Category{" "}
          <select
            value={category}
            onChange={(e) => setCategory(e.target.value)}
          >
            <option value="all">All categories</option>
            {categories.map((c) => (
              <option key={c}>{c}</option>
            ))}
          </select>
        </label>
      </div>
      {error ? (
        <ErrorBox error={error} onRetry={() => setRevision((r) => r + 1)} />
      ) : loading ? (
        <Loading />
      ) : visible.length ? (
        <div className="directory-grid">
          {visible.map((entry) => {
            const link = sourceLink(entry.source_url);
            const endpoint =
              entry.template?.transport === "stdio"
                ? entry.template.command
                : entry.template?.url;
            return (
              <article className="directory-card" key={entry.id}>
                <div className="directory-card-top">
                  <span
                    className={`connection-icon ${entry.template?.transport === "stdio" ? "peach" : "mint"}`}
                  >
                    {entry.template?.transport === "stdio" ? (
                      <Monitor size={22} />
                    ) : (
                      <Globe size={22} />
                    )}
                  </span>
                  <span className="small-pill">
                    {entry.source === "org" ? (
                      <>
                        <Users size={12} /> Organization
                      </>
                    ) : (
                      "Community"
                    )}
                  </span>
                </div>
                <div>
                  <div className="directory-category">{entry.category}</div>
                  <h2>{entry.name}</h2>
                </div>
                <p className="directory-description">
                  {entry.description ||
                    "Review this MCP’s source and configure its endpoint to get started."}
                </p>
                <div className="directory-endpoint">
                  <span>
                    {entry.template?.transport === "stdio"
                      ? "Command on your host"
                      : "MCP endpoint"}
                  </span>
                  <code>
                    {endpoint ||
                      (entry.template?.transport === "stdio"
                        ? "Choose an absolute command path during setup"
                        : "Enter endpoint during setup")}
                  </code>
                </div>
                <div className="directory-source">
                  {link ? (
                    <a href={link} target="_blank" rel="noreferrer">
                      {entry.source === "community"
                        ? "View community listing"
                        : "View source"}{" "}
                      <ExternalLink size={13} />
                    </a>
                  ) : (
                    <span>No source link provided</span>
                  )}
                  {entry.source === "org" && (
                    <small>Added by {entry.owner_id}</small>
                  )}
                </div>
                <div className="directory-card-actions">
                  <Button variant="secondary" onClick={() => onUse(entry)}>
                    Use this MCP <ArrowRight size={15} />
                  </Button>
                  {entry.can_manage && entry.source === "org" && (
                    <div>
                      <button
                        className="icon-button"
                        aria-label={`Edit ${entry.name}`}
                        onClick={() => setEditor(entry)}
                      >
                        <Pencil size={16} />
                      </button>
                      <button
                        className="icon-button"
                        aria-label={`Delete ${entry.name}`}
                        onClick={() => {
                          setDeleteError("");
                          setDeleting(entry);
                        }}
                      >
                        <Trash2 size={16} />
                      </button>
                    </div>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      ) : (
        <Empty
          icon={<BookOpen size={24} />}
          title="No directory entries found"
          description="Try another search, add an organization entry, or configure your own endpoint."
          action={
            <Button variant="secondary" onClick={onCustom}>
              Use a custom endpoint <ArrowRight size={15} />
            </Button>
          }
        />
      )}
      <p className="directory-footnote">
        Community entries are a built-in snapshot attributed to mcpservers.org.
        Check the linked source for current setup instructions. Organization
        entries are visible to your organization; each connection has its own
        access settings.
      </p>
      {editor && (
        <DirectoryEditor
          entry={editor === "new" ? null : editor}
          onClose={() => setEditor(null)}
          onSaved={() => {
            setEditor(null);
            setRevision((r) => r + 1);
            notify("Organization directory entry saved.");
          }}
        />
      )}
      <Dialog
        open={!!deleting}
        onOpenChange={(open) => {
          if (!open && !busy) setDeleting(null);
        }}
      >
        <DialogContent
          title="Remove directory entry?"
          description={`Remove ${deleting?.name || "this entry"} from your organization’s directory. Existing connections will keep their configuration and access.`}
        >
          <div className="form-stack">
            {deleteError && <ErrorBox error={deleteError} />}
            <div className="form-actions">
              <Button
                variant="secondary"
                disabled={busy}
                onClick={() => setDeleting(null)}
              >
                Keep entry
              </Button>
              <Button loading={busy} onClick={() => void remove()}>
                Remove entry
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}

function DirectoryEditor({
  entry,
  onClose,
  onSaved,
}: {
  entry: DirectoryEntry | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState(entry?.name ?? "");
  const [description, setDescription] = useState(entry?.description ?? "");
  const [category, setCategory] = useState(entry?.category ?? "Other");
  const [source, setSource] = useState(entry?.source_url ?? "");
  const [transport, setTransport] = useState(
    entry?.template?.transport ?? "reference",
  );
  const [url, setUrl] = useState(entry?.template?.url ?? "");
  const [command, setCommand] = useState(entry?.template?.command ?? "");
  const [args, setArgs] = useState(entry?.template?.args.join("\n") ?? "");
  const [auth, setAuth] = useState<"none" | "per-user" | "shared">(
    entry?.template?.auth_mode ?? "none",
  );
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function save() {
    setError("");
    if (!name.trim()) {
      setError("Give the directory entry a name.");
      return;
    }
    for (const [label, value] of [
      ["Source link", source],
      ["MCP endpoint", transport === "http" ? url : ""],
    ]) {
      if (!value.trim()) continue;
      try {
        const u = new URL(value);
        if (!sourceLink(value) || u.search || u.hash) throw Error();
      } catch {
        setError(
          `${label} must be an HTTP or HTTPS URL without credentials, query parameters or a fragment.`,
        );
        return;
      }
    }
    const input: DirectoryInput = {
      name: name.trim(),
      description: description.trim(),
      category: category.trim() || "Other",
      source_url: source.trim() || null,
      template:
        transport === "reference"
          ? null
          : {
              transport: transport as "http" | "stdio",
              url: transport === "http" ? url.trim() || null : null,
              command: transport === "stdio" ? command.trim() || null : null,
              args:
                transport === "stdio" ? args.split("\n").filter(Boolean) : [],
              auth_mode: auth,
            },
    };
    setBusy(true);
    try {
      if (entry) await api.updateDirectoryEntry(entry.id, input, entry.version);
      else await api.createDirectoryEntry(input);
      onSaved();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent
        className="wide-dialog"
        title={entry ? "Edit organization entry" : "Add organization entry"}
        description="Share reusable setup details with your organization. Only you can edit or remove your entry."
      >
        <form
          className="form-stack"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          {error && <ErrorBox error={error} />}
          <Input
            autoFocus
            label="Name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={100}
            placeholder="e.g. Team documentation"
          />
          <Textarea
            label="Description"
            value={description}
            onChange={setDescription}
            rows={2}
            placeholder="What can this MCP do?"
          />
          <Input
            label="Category"
            value={category}
            onChange={(e) => setCategory(e.target.value)}
            maxLength={64}
            placeholder="e.g. Developer tools"
          />
          <Input
            label="Source link (optional)"
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder="https://provider.example.com/mcp-guide"
            description="Link to the provider or setup instructions."
          />
          <Select
            label="Setup template"
            value={transport}
            onChange={setTransport}
          >
            <option value="reference">
              Reference only — enter setup later
            </option>
            <option value="http">HTTP endpoint</option>
            <option value="stdio">Local stdio process</option>
          </Select>
          {transport === "http" && (
            <Input
              label="MCP endpoint (optional)"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://mcp.example.com/mcp"
              description="Leave blank when each user must supply their own endpoint."
            />
          )}
          {transport === "stdio" && (
            <>
              <Input
                label="Command (optional)"
                value={command}
                onChange={(e) => setCommand(e.target.value)}
                placeholder="/absolute/path/to/mcp-server"
                description="The user selects their own host and reviews the absolute executable path during setup."
              />
              <Textarea
                label="Arguments (one per line)"
                value={args}
                onChange={setArgs}
                rows={3}
                code
              />
            </>
          )}
          {transport !== "reference" && (
            <Select
              label="Provider account setup"
              value={auth}
              onChange={(v) => setAuth(v as typeof auth)}
            >
              <option value="none">No authentication</option>
              <option value="per-user">
                Each user connects their own account
              </option>
              <option value="shared">Use one shared account</option>
            </Select>
          )}
          <Notice>
            Directory entries contain no accounts or secrets. Keep tokens,
            passwords and environment variables out of URLs, commands and
            arguments. Authentication happens when configuring a connection.
          </Notice>
          <div className="form-actions">
            <Button
              type="button"
              variant="ghost"
              disabled={busy}
              onClick={onClose}
            >
              Cancel
            </Button>
            <Button type="submit" loading={busy}>
              Save entry
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
