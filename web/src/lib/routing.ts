import { useCallback, useEffect, useState } from "react";

export type Page =
  "connections" | "directory" | "hosts" | "activity" | "settings" | "help";
export type ConnectionTab =
  "tools" | "account" | "access" | "resources" | "settings";
export type Route = {
  page: Page;
  connectionId?: string;
  tab?: ConnectionTab;
  callId?: string;
};
const pages = new Set<Page>([
  "connections",
  "directory",
  "hosts",
  "activity",
  "settings",
  "help",
]);
const tabs = new Set<ConnectionTab>([
  "tools",
  "account",
  "access",
  "resources",
  "settings",
]);
export function parseRoute(pathname: string): Route {
  let parts: string[];
  try {
    parts = pathname.split("/").filter(Boolean).map(decodeURIComponent);
  } catch {
    return { page: "connections" };
  }
  const page = parts[0] as Page;
  if (!pages.has(page)) return { page: "connections" };
  if (page === "connections" && parts[1])
    return {
      page,
      connectionId: parts[1],
      tab: tabs.has(parts[2] as ConnectionTab)
        ? (parts[2] as ConnectionTab)
        : "tools",
    };
  if (page === "activity" && parts[1]) return { page, callId: parts[1] };
  return { page };
}
export function routePath(route: Route): string {
  if (route.page === "connections" && route.connectionId)
    return `/connections/${encodeURIComponent(route.connectionId)}${route.tab && route.tab !== "tools" ? `/${route.tab}` : ""}`;
  if (route.page === "activity" && route.callId)
    return `/activity/${encodeURIComponent(route.callId)}`;
  return `/${route.page}`;
}
export function useRoute() {
  const [route, setRoute] = useState(() =>
    parseRoute(window.location.pathname),
  );
  useEffect(() => {
    const onPop = () => setRoute(parseRoute(window.location.pathname));
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const go = useCallback((next: Route) => {
    const path = routePath(next);
    if (window.location.pathname !== path)
      window.history.pushState(null, "", path);
    setRoute(next);
  }, []);
  return [route, go] as const;
}
