/** The sole boundary between the website and MCPort's authenticated API. */
import { ensureFreshSession, markSignedOut } from "@/lib/client/session";
export type AccountRef={uuid:string;id:string;kind:string;display_name:string;pfp_url?:string};
export type Session=AccountRef & {expires_at:number;custodian?:AccountRef|null};
export type Discovery={app_id:string;accounts_url:string;backend_url:string;website_url:string;repository_url:string;docs_url:string;package_url:string};
export type Account = {
  connected: boolean;
  account?: AccountRef | null;
  label: string;
  kind: string;
};
export type Connection = {
  id: string;
  name: string;
  description: string;
  owner: AccountRef;
  visibility: "circle" | "invited";
  transport: "http" | "stdio";
  url?: string;
  host_id?: string;
  command?: string;
  args: string[];
  auth_mode: "none" | "per-user" | "shared";
  status: string;
  can_manage: boolean;
  account?: Account;
  created_at: number;
  updated_at: number;
  version: number;
};
export type ConnectionInput = Pick<
  Connection,
  "name" | "description" | "transport" | "auth_mode" | "visibility" | "args"
> &
  Partial<Pick<Connection, "url" | "host_id" | "command">>;
export type DirectoryTemplate = {
  transport: Connection["transport"];
  url: string | null;
  command: string | null;
  args: string[];
  auth_mode: Connection["auth_mode"];
};
export type DirectoryInput = {
  name: string;
  description: string;
  category: string;
  source_url: string | null;
  template: DirectoryTemplate | null;
};
export type DirectoryEntry = DirectoryInput & {
  id: string;
  source: "community" | "personal";
  owner: AccountRef;
  can_manage: boolean;
  source_revision: string | null;
  version: number;
  created_at: number;
  updated_at: number;
};
export type Host = {
  id: string;
  name: string;
  online: boolean;
  last_seen?: number;
  owner: AccountRef;
  can_manage: boolean;
};
export type Tool = {
  name: string;
  description?: string;
  inputSchema: Record<string, unknown>;
  enabled: boolean;
  annotations?: Record<string, unknown>;
};
export type Access = { account:AccountRef; created_at:number;created_by?:AccountRef|null };
export type Policy = {
  tool: string;
  account: AccountRef | null;
  enabled: boolean;
};
export type Activity = {
  id: string;
  connection_id: string;
  connection_name: string;
  caller: AccountRef;
  execution_account: AccountRef|null;
  method: string;
  tool_name?: string;
  status: string;
  created_at: number;
  completed_at?: number;
  result?: unknown;
  error?: { message: string; recovery?: string; outcome_unknown: boolean };
};
export type ResultAsset = {
  index: number;
  name: string;
  mime_type: string;
  size: number;
  source_uri?: string;
  download_url: string;
};
export type CallResult = { call_id: string; result: Record<string, unknown> };
export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
    public recovery?: string,
    public outcomeUnknown = false,
  ) {
    super(message);
  }
}
async function request<T>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  await ensureFreshSession();
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  let response: Response;
  try {
    response = await fetch(`/api/api/v1${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: "same-origin",
    });
  } catch {
    const uncertain =
      path.endsWith("/mcp") &&
      (body as { method?: string } | undefined)?.method === "tools/call";
    throw new ApiError(
      0,
      "unreachable",
      "MCPort could not be reached. Check your connection.",
      uncertain
        ? "Inspect Activity and the provider before retrying this action."
        : "Try again when the connection returns.",
      uncertain,
    );
  }
  if (response.status === 401) markSignedOut();
  if (response.status === 204) return undefined as T;
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    const e = data?.error;
    const proxyOutcomeUnknown =
      (response.status === 502 || response.status === 504) &&
      path.endsWith("/mcp") &&
      (body as { method?: string } | undefined)?.method === "tools/call" &&
      !(typeof e?.code === "string" && typeof e?.message === "string");
    if (proxyOutcomeUnknown) {
      throw new ApiError(
        response.status,
        "proxy_outcome_unknown",
        "The gateway could not return the tool response.",
        "Inspect Activity and the provider before retrying this action.",
        true,
      );
    }
    throw new ApiError(
      response.status,
      e?.code || "request_failed",
      e?.message || `Request failed (${response.status}).`,
      e?.recovery,
      e?.outcome_unknown,
    );
  }
  return data?.data as T;
}
async function downloadAsset(call:string,index:number):Promise<string> {
 if(!Number.isSafeInteger(index)||index<0)throw new Error("Invalid result file.");
 const ticket=await request<{url:string}>("/calls/"+encodeURIComponent(call)+"/assets/"+index+"/ticket","POST");
 return ticket.url;
}
const id = encodeURIComponent;
type WebOperation =
  | "navigation"
  | "login"
  | "connection.create"
  | "tool.list"
  | "tool.call"
  | "resource.list"
  | "resource.read"
  | "prompt.list"
  | "prompt.get";
let webTelemetryEnabled = true;
export function recordWebEvent(
  operation: WebOperation,
  step: "start" | "complete" | "render",
  outcome: "pending" | "success" | "failure",
  correlation_id?: string,
  duration_ms?: number,
) {
  if (!webTelemetryEnabled) return;
  void request("/telemetry", "POST", {
    source: "web",
    operation,
    step,
    outcome,
    progress: step === "start" ? 0 : 1,
    correlation_id,
    duration_ms,
  }).catch(() => {});
}
const rpc = async (
  connection: string,
  method: string,
  params: unknown = {},
) => {
  const operations: Record<string, WebOperation> = {
    "tools/list": "tool.list",
    "tools/call": "tool.call",
    "resources/list": "resource.list",
    "resources/templates/list": "resource.list",
    "resources/read": "resource.read",
    "prompts/list": "prompt.list",
    "prompts/get": "prompt.get",
  };
  const operation = operations[method];
  const correlation = crypto.randomUUID();
  const start = performance.now();
  if (operation) recordWebEvent(operation, "start", "pending", correlation);
  try {
    const result = await request<CallResult>(
      `/connections/${id(connection)}/mcp`,
      "POST",
      { method, params, timeout_ms: 60000 },
    );
    if (operation)
      recordWebEvent(
        operation,
        "complete",
        result.result.isError ? "failure" : "success",
        correlation,
        Math.round(performance.now() - start),
      );
    return result;
  } catch (error) {
    if (operation)
      recordWebEvent(
        operation,
        "complete",
        "failure",
        correlation,
        Math.round(performance.now() - start),
      );
    throw error;
  }
};
async function listTools(connection: string) {
  const tools: Tool[] = [];
  const seen = new Set<string>();
  let cursor: string | undefined;
  do {
    const r = await rpc(connection, "tools/list", cursor ? { cursor } : {});
    tools.push(...((r.result.tools || []) as Tool[]));
    cursor =
      typeof r.result.nextCursor === "string" ? r.result.nextCursor : undefined;
    if (cursor) {
      if (seen.has(cursor))
        throw new ApiError(
          502,
          "pagination_loop",
          "The MCP repeated a discovery cursor.",
          "Refresh after the provider resolves its pagination error.",
        );
      seen.add(cursor);
    }
  } while (cursor);
  return tools;
}
export const api = {
  discovery: () => request<Discovery>("/discovery"),
  me: () => request<Session>("/me"),
  deleteHost: (host:string) => request<void>("/hosts/"+id(host),"DELETE"),
  directory: (query = "") =>
    request<DirectoryEntry[]>(
      `/directory${query ? `?q=${encodeURIComponent(query)}` : ""}`,
    ),
  directoryEntry: (entry: string) =>
    request<DirectoryEntry>(`/directory/${id(entry)}`),
  createDirectoryEntry: (input: DirectoryInput) =>
    request<DirectoryEntry>("/directory", "POST", input),
  updateDirectoryEntry: (
    entry: string,
    input: DirectoryInput,
    version: number,
  ) =>
    request<DirectoryEntry>(`/directory/${id(entry)}`, "PUT", {
      input,
      version,
    }),
  deleteDirectoryEntry: (entry: string) =>
    request<{ deleted: boolean }>(`/directory/${id(entry)}`, "DELETE"),
  connections: () => request<Connection[]>("/connections"),
  connection: (connection: string) =>
    request<Connection>(`/connections/${id(connection)}`),
  createConnection: (data: ConnectionInput) =>
    request<Connection>("/connections", "POST", data),
  updateConnection: (
    connection: string,
    data: Partial<
      Pick<Connection, "name" | "description" | "visibility" | "version">
    >,
  ) => request<Connection>(`/connections/${id(connection)}`, "PATCH", data),
  deleteConnection: (connection: string) =>
    request<void>(`/connections/${id(connection)}`, "DELETE"),
  tools: listTools,
  refreshTools: listTools,
  policies: (connection: string) =>
    request<Policy[]>(`/connections/${id(connection)}/policies`),
  setTool: (
    connection: string,
    tool: string,
    enabled: boolean,
    principal_id?: string,
  ) =>
    request<Policy>(`/connections/${id(connection)}/policies`, "PUT", {
      tool,
      enabled,
      account: principal_id || null,
    }),
  call: (connection: string, tool: string, input: unknown) =>
    rpc(connection, "tools/call", { name: tool, arguments: input }),
  access: (connection: string) =>
    request<Access[]>(`/connections/${id(connection)}/access`),
  invite: (connection: string, principal_id: string) =>
    request<Access>(`/connections/${id(connection)}/access`, "POST", {
      account: principal_id,
    }),
  revoke: (connection: string, principal: string) =>
    request<void>(
      `/connections/${id(connection)}/access/${id(principal)}`,
      "DELETE",
    ),
  account: (connection: string) =>
    request<Account>(`/connections/${id(connection)}/account`),
  connectAccount: (
    connection: string,
    secret: string,
    label: string,
    kind = "bearer",
    header_name?: string,
  ) =>
    request<Account>(`/connections/${id(connection)}/account`, "POST", {
      kind,
      secret,
      label,
      header_name,
    }),
  authorizeAccount: (connection: string, client_id?: string) =>
    request<{ authorization_url: string; state: string }>(
      `/connections/${id(connection)}/account/authorize`,
      "POST",
      { client_id },
    ),
  disconnectAccount: (connection: string) =>
    request<void>(`/connections/${id(connection)}/account`, "DELETE"),
  hosts: () => request<Host[]>("/hosts"),
  activity: () => request<Activity[]>("/calls"),
  invocation: (call: string) => request<Activity>(`/calls/${id(call)}`),
  assets: (call: string) => request<ResultAsset[]>(`/calls/${id(call)}/assets`),
  downloadAsset,
  cancel: (call: string) =>
    request<Activity>(`/calls/${id(call)}/cancel`, "POST"),
  settings: async () => {
    const result = await request<{ telemetry: boolean }>("/settings");
    webTelemetryEnabled = result.telemetry;
    return result;
  },
  setSettings: async (telemetry: boolean) => {
    const result = await request<{ telemetry: boolean }>("/settings", "PATCH", {
      telemetry,
    });
    webTelemetryEnabled = result.telemetry;
    return result;
  },
  rpc,
  report: (message: string, pr?: string) =>
    request<{ id: string; status: string; repository_url: string }>(
      "/reports",
      "POST",
      { message, pr },
    ),
};
export const message = (e: unknown) =>
  e instanceof ApiError
    ? [
        e.message,
        e.recovery,
        e.outcomeUnknown
          ? "The action may have completed. Check the provider before retrying."
          : null,
      ]
        .filter(Boolean)
        .join(" ")
    : e instanceof Error
      ? e.message
      : "Something went wrong. Please try again.";
