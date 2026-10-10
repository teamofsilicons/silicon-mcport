import type { Connection, ConnectionInput, DirectoryEntry } from "./api";

export const defaultVisibility = (
  auth: Connection["auth_mode"],
): "circle" | "invited" => (auth === "none" ? "circle" : "invited");

export function changeAuthentication(
  data: ConnectionInput,
  auth: Connection["auth_mode"],
  explicitVisibility: boolean,
): ConnectionInput {
  return {
    ...data,
    auth_mode: auth,
    visibility: explicitVisibility ? data.visibility : defaultVisibility(auth),
  };
}

export function connectionFromDirectory(
  entry?: DirectoryEntry | null,
): ConnectionInput {
  const template = entry?.template;
  const auth_mode = template?.auth_mode ?? "none";
  // Deliberately copy only setup fields. Directory ownership grants no access,
  // and no template can select a user's execution host or provider credentials.
  return {
    name: entry ? connectionName(entry.name) : "",
    description: entry?.description ?? "",
    transport: template?.transport ?? "http",
    auth_mode,
    visibility: defaultVisibility(auth_mode),
    url: template?.url ?? undefined,
    command: template?.command ?? undefined,
    args: [...(template?.args ?? [])],
  };
}

export function connectionName(name: string): string {
  return (
    name
      .normalize("NFKD")
      .replace(/[\u0300-\u036f]/g, "")
      .replace(/[^a-zA-Z0-9_-]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 80) || "mcp"
  );
}

export function changeTransport(
  data: ConnectionInput,
  transport: Connection["transport"],
): ConnectionInput {
  return transport === "http"
    ? { ...data, transport, command: undefined, args: [] }
    : { ...data, transport, url: undefined };
}

export function isAbsoluteCommand(command: string): boolean {
  return (
    command.startsWith("/") ||
    /^[a-z]:[\\/]/i.test(command) ||
    /^\\\\[^\\]+\\[^\\]+/.test(command)
  );
}

export function sourceLink(value?: string | null): string | undefined {
  if (!value) return;
  try {
    const url = new URL(value);
    if (
      ["https:", "http:"].includes(url.protocol) &&
      !url.username &&
      !url.password
    )
      return url.href;
  } catch {
    /* Render untrusted source metadata as plain text only. */
  }
}
