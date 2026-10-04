import { describe, expect, it } from "vitest";
import { deploymentConfig } from "../scripts/build-vercel.mjs";

describe("Vercel deployment boundary", () => {
  it("requires an explicit HTTPS backend origin and rejects embedded credentials or paths", () => {
    for (const value of [
      undefined,
      "",
      "http://127.0.0.1:4380",
      "https://user:secret@example.com",
      "https://example.com/api",
      "https://example.com/?x=1",
      "https://example.com/#x",
    ]) {
      expect(() => deploymentConfig(value)).toThrow();
    }
    expect(deploymentConfig("https://backend.example/").version).toBe(3);
  });
  it("routes all API paths to the configured backend before static or SPA handling and disables caching", () => {
    const { routes } = deploymentConfig("https://backend.example");
    const api = routes.find((route) => route.dest?.startsWith("https:"));
    expect(routes.indexOf(api)).toBeLessThan(
      routes.findIndex((route) => route.handle === "filesystem"),
    );
    for (const path of [
      "/api",
      "/api/v1/auth/browser/complete",
      "/api/v1/calls/call/assets/0",
    ]) {
      expect(path.replace(new RegExp(`^${api.src}$`), api.dest)).toBe(
        `https://backend.example${path}`,
      );
    }
    expect(new RegExp(`^${api.src}$`).test("/api-other")).toBe(false);
    expect(api.headers["Vercel-CDN-Cache-Control"]).toBe("no-store");
    expect(api.headers["Cache-Control"]).toBe("private, no-store");
  });
  it("supports callback and workspace deep links while missing chunks stay 404", () => {
    const { routes } = deploymentConfig("https://backend.example");
    const afterFilesystem = routes.slice(
      routes.findIndex((route) => route.handle === "filesystem") + 1,
    );
    const routeFor = (path) =>
      afterFilesystem.find((route) => new RegExp(`^${route.src}$`).test(path));
    expect(routeFor("/assets/missing.js").status).toBe(404);
    for (const path of [
      "/auth/callback",
      "/connections/id/access",
      "/activity/call",
      "/settings",
    ]) {
      expect(routeFor(path).dest).toBe("/index.html");
      expect(routeFor(path).methods).toEqual(["GET", "HEAD"]);
    }
  });
});
