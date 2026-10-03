import { describe, expect, it } from "vitest";
import { parseRoute, routePath } from "../src/lib/routing";
describe("workspace deep links", () => {
  it("preserves connection and selected section across refreshes", () => {
    const route = {
      page: "connections" as const,
      connectionId: "team/design",
      tab: "access" as const,
    };
    expect(routePath(route)).toBe("/connections/team%2Fdesign/access");
    expect(parseRoute(routePath(route))).toEqual(route);
    expect(parseRoute("/connections/id")).toEqual({
      page: "connections",
      connectionId: "id",
      tab: "tools",
    });
  });
  it("preserves result links and each workspace page", () => {
    const route = { page: "activity" as const, callId: "call-1" };
    expect(parseRoute(routePath(route))).toEqual(route);
    for (const page of [
      "connections",
      "hosts",
      "activity",
      "settings",
      "help",
    ] as const) {
      expect(parseRoute(routePath({ page }))).toEqual({ page });
    }
  });
  it("treats malformed and unknown locations as a recoverable catalog entry", () => {
    expect(parseRoute("/connections/%ZZ")).toEqual({ page: "connections" });
    expect(parseRoute("/unknown")).toEqual({ page: "connections" });
    expect(parseRoute("/")).toEqual({ page: "connections" });
    expect(parseRoute("/connections/id/unknown").tab).toBe("tools");
  });
});
