import { describe, expect, it } from "vitest";
import type { DirectoryEntry } from "../src/lib/api";
import {
  changeAuthentication,
  changeTransport,
  connectionFromDirectory,
  connectionName,
  isAbsoluteCommand,
  sourceLink,
} from "../src/lib/directory";
const entry: DirectoryEntry = {
  id: "org-template",
  name: "Team GitHub ✨ MCP",
  description: "Repository tools",
  category: "Development",
  source: "org",
  source_url: "https://github.com/github/github-mcp-server",
  source_revision: null,
  owner_id: "c:owner",
  org_id: "tos",
  environment: "production",
  can_manage: false,
  version: 7,
  created_at: 0,
  updated_at: 0,
  template: {
    transport: "stdio",
    url: null,
    command: null,
    args: ["stdio"],
    auth_mode: "per-user",
  },
};
describe("directory setup and connection access", () => {
  it("defaults unauthenticated MCPs to org and account-backed MCPs to invited", () => {
    const custom = connectionFromDirectory();
    expect(custom.visibility).toBe("org");
    const personal = changeAuthentication(custom, "per-user", false);
    expect(personal.visibility).toBe("invited");
    expect(changeAuthentication(personal, "shared", false).visibility).toBe(
      "invited",
    );
    expect(changeAuthentication(personal, "none", false).visibility).toBe(
      "org",
    );
  });
  it("never widens an explicit invite-only choice when removing authentication", () => {
    const chosen = {
      ...connectionFromDirectory(),
      visibility: "invited" as const,
    };
    expect(changeAuthentication(chosen, "none", true).visibility).toBe(
      "invited",
    );
    expect(
      changeAuthentication({ ...chosen, visibility: "org" }, "per-user", true)
        .visibility,
    ).toBe("org");
  });
  it("prefills setup without copying directory ownership, host, access or credentials", () => {
    const setup = connectionFromDirectory({
      ...entry,
      host_id: "someone-elses-host",
      visibility: "org",
      credentials: "never-copy",
    } as DirectoryEntry);
    expect(setup).toEqual({
      name: "Team-GitHub-MCP",
      description: "Repository tools",
      transport: "stdio",
      auth_mode: "per-user",
      visibility: "invited",
      url: undefined,
      command: undefined,
      args: ["stdio"],
    });
    setup.args.push("changed");
    expect(entry.template?.args).toEqual(["stdio"]);
  });
  it("requires machine-specific input for reference-only entries and normalizes source names", () => {
    expect(connectionFromDirectory({ ...entry, template: null })).toMatchObject(
      {
        name: "Team-GitHub-MCP",
        transport: "http",
        url: undefined,
        command: undefined,
        visibility: "org",
      },
    );
    expect(connectionName("🔥 中文")).toBe("mcp");
    expect(connectionName("a".repeat(200))).toHaveLength(80);
    expect(connectionName("Mémory / Tools")).toBe("Memory-Tools");
  });
  it("removes incompatible arguments and endpoint fields on a transport change", () => {
    const stdio = {
      ...connectionFromDirectory(entry),
      command: "/opt/mcp",
      args: ["stdio", "--flag"],
    };
    expect(changeTransport(stdio, "http")).toMatchObject({
      transport: "http",
      command: undefined,
      args: [],
    });
    expect(
      changeTransport(
        {
          ...stdio,
          transport: "http",
          url: "https://mcp.example/mcp",
          args: [],
        },
        "stdio",
      ),
    ).toMatchObject({ transport: "stdio", url: undefined });
  });
  it("does not accept a bare executable as an absolute local command", () => {
    for (const value of ["mcp", "npx", "./mcp", "", "C:relative.exe"])
      expect(isAbsoluteCommand(value)).toBe(false);
    for (const value of [
      "/opt/bin/mcp",
      "C:\\tools\\mcp.exe",
      "\\\\host\\share\\mcp.exe",
    ])
      expect(isAbsoluteCommand(value)).toBe(true);
  });
  it("does not turn executable or credential-bearing metadata into source links", () => {
    expect(sourceLink("javascript:alert(1)")).toBeUndefined();
    expect(sourceLink("https://user:secret@example.com")).toBeUndefined();
    expect(sourceLink("https://mcpservers.org/servers/example")).toBe(
      "https://mcpservers.org/servers/example",
    );
  });
});
