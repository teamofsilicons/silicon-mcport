import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  api,
  ApiError,
  message,
  saveSession,
  savedSession,
} from "../src/lib/api";
const actor = {
  principal_id: "si:test",
  identity_kind: "silicon" as const,
  org_id: "test-org",
  display_name: "Test Silicon",
};
const session = { actor, environment: "production", expires_at: 2000000000 };
const response = (data: unknown, status = 200) =>
  new Response(JSON.stringify(status === 200 ? { data } : { error: data }), {
    status,
    headers: { "content-type": "application/json" },
  });
let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
  const storage = new Map<string, string>();
  vi.stubGlobal("sessionStorage", {
    getItem: (k: string) => storage.get(k) || null,
    setItem: (k: string, v: string) => storage.set(k, v),
    removeItem: (k: string) => storage.delete(k),
  });
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => vi.unstubAllGlobals());
describe("browser API security and contract", () => {
  it("uses same-origin cookies and sends validated environment metadata without JS bearer tokens", async () => {
    saveSession({ ...session, environment: "test-one" });
    fetchMock.mockResolvedValueOnce(response([]));
    await api.connections();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/connections",
      expect.objectContaining({
        credentials: "same-origin",
        headers: { Accept: "application/json", "X-MCPort-Test": "test-one" },
      }),
    );
    expect(savedSession()).toEqual({ ...session, environment: "test-one" });
  });
  it("exchanges a manual SLT through a typed browser attempt", async () => {
    fetchMock
      .mockResolvedValueOnce(
        response({ state: "nonce", url: "https://iam.example/login" }),
      )
      .mockResolvedValueOnce(response(session));
    expect(await api.login("slt-example", "silicon")).toEqual(session);
    expect(fetchMock.mock.calls[0][0]).toBe(
      "/api/v1/auth/browser/start?identity_kind=silicon",
    );
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({
      slt: "slt-example",
      state: "nonce",
    });
    expect(sessionStorage.getItem("mcport.session.v1")).toBeNull();
  });
  it("preserves upstream RPC arguments and encodes connection identifiers", async () => {
    fetchMock.mockResolvedValueOnce(
      response({ call_id: "call-1", result: { content: [] } }),
    );
    await api.call("shared/design", "create_asset", {
      nested: { values: [1, 2] },
    });
    expect(fetchMock.mock.calls[0][0]).toBe(
      "/api/v1/connections/shared%2Fdesign/mcp",
    );
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      method: "tools/call",
      params: {
        name: "create_asset",
        arguments: { nested: { values: [1, 2] } },
      },
      timeout_ms: 60000,
    });
  });
  it("paginates tool discovery and rejects repeated cursors", async () => {
    fetchMock
      .mockResolvedValueOnce(
        response({
          call_id: "1",
          result: { tools: [{ name: "a" }], nextCursor: "next" },
        }),
      )
      .mockResolvedValueOnce(
        response({ call_id: "2", result: { tools: [{ name: "b" }] } }),
      );
    expect((await api.tools("docs")).map((t) => t.name)).toEqual(["a", "b"]);
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).params).toEqual({
      cursor: "next",
    });
    fetchMock.mockImplementation(() =>
      Promise.resolve(
        response({ call_id: "3", result: { tools: [], nextCursor: "loop" } }),
      ),
    );
    await expect(api.tools("docs")).rejects.toMatchObject({
      code: "pagination_loop",
    });
  });
  it("does not replay a tool call after a lost response and marks its outcome unknown", async () => {
    fetchMock.mockRejectedValue(new TypeError("Failed to fetch"));
    const error = await api.call("docs", "write", {}).catch((e) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error.outcomeUnknown).toBe(true);
    expect(message(error)).toContain("may have completed");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
  it("preserves API denial recovery information", async () => {
    fetchMock.mockResolvedValueOnce(
      response(
        {
          code: "tool_disabled",
          message: "Tool is disabled.",
          recovery: "Ask the owner to enable it.",
          outcome_unknown: false,
        },
        403,
      ),
    );
    await expect(api.call("docs", "write", {})).rejects.toMatchObject({
      status: 403,
      code: "tool_disabled",
      recovery: "Ask the owner to enable it.",
    });
  });
  it("serializes simultaneous refreshes so a single cookie is rotated once", async () => {
    fetchMock.mockResolvedValueOnce(response(session));
    const [a, b] = await Promise.all([
      api.browserRefresh(),
      api.browserRefresh(),
    ]);
    expect(a).toEqual(session);
    expect(b).toEqual(session);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
  it("fetches full call results and downloads bytes with current cookie and environment authorization", async () => {
    saveSession({ ...session, environment: "asset-test" });
    fetchMock
      .mockResolvedValueOnce(
        response({ id: "call-1", result: { content: [] } }),
      )
      .mockResolvedValueOnce(response([{ index: 1, name: "design.svg" }]))
      .mockResolvedValueOnce(
        new Response("<svg/>", {
          headers: { "content-type": "application/octet-stream" },
        }),
      );
    expect((await api.invocation("call-1")).id).toBe("call-1");
    expect((await api.assets("call-1"))[0].name).toBe("design.svg");
    const blob = await api.downloadAsset("call-1", 1);
    expect(await blob.text()).toBe("<svg/>");
    expect(blob.type).toBe("application/octet-stream");
    expect(fetchMock.mock.calls[2]).toEqual([
      "/api/v1/calls/call-1/assets/1",
      {
        credentials: "same-origin",
        redirect: "error",
        headers: {
          Accept: "application/octet-stream",
          "X-MCPort-Test": "asset-test",
        },
      },
    ]);
  });
  it("preserves download denial and rejects invalid indices before network access", async () => {
    await expect(api.downloadAsset("call", -1)).rejects.toMatchObject({
      code: "invalid_asset",
    });
    expect(fetchMock).not.toHaveBeenCalled();
    fetchMock.mockResolvedValueOnce(
      response(
        { code: "access_denied", message: "Tool access was revoked." },
        403,
      ),
    );
    await expect(api.downloadAsset("call", 0)).rejects.toMatchObject({
      code: "access_denied",
      status: 403,
    });
  });
  it("stops oversized file downloads at the size boundary", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("too large", { headers: { "content-length": "16777217" } }),
    );
    await expect(api.downloadAsset("call", 0)).rejects.toMatchObject({
      code: "asset_too_large",
    });
  });
});
