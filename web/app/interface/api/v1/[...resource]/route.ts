// A standalone interface supplies its own Accounts bearer. This transport has
// no browser session, app secret exchange, cookies, or user-selectable upstream.
import { type NextRequest } from "next/server";
import { serverEnv } from "@/lib/server/env";
import {
  forwardRequestHeaders,
  relayResponseHeaders,
  upstreamPath,
} from "@/lib/server/forward";

export const dynamic = "force-dynamic";
export const maxDuration = 300;
const prefix = "/interface/api/v1/";
const cors = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods":
    "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS",
  "access-control-allow-headers":
    "Authorization, Content-Type, Accept, Idempotency-Key, If-Match, Range",
  "access-control-expose-headers":
    "Content-Disposition, Content-Range, Retry-After",
  "cache-control": "private, no-store",
};
export function OPTIONS() {
  return new Response(null, { status: 204, headers: cors });
}
async function handle(request: NextRequest) {
  const resource = upstreamPath(request.nextUrl.pathname.slice(prefix.length));
  const error = (status: number, code: string, message: string) =>
    Response.json(
      { error: { code, message, recovery: null, outcome_unknown: false } },
      { status, headers: cors },
    );
  if (!resource)
    return error(400, "invalid_path", "Invalid McPort API resource.");
  const raw = request.headers.get("authorization") ?? "";
  const token = /^Bearer (\S+)$/i.exec(raw)?.[1];
  const isPublic =
    resource === "discovery" || resource.startsWith("downloads/");
  if (!token && !isPublic)
    return error(
      401,
      "authentication_required",
      "Supply the Accounts access token for mcport.",
    );
  const headers = forwardRequestHeaders(request.headers, token ?? "", {});
  if (!token) headers.delete("authorization");
  const hasBody =
    request.body !== null &&
    request.method !== "GET" &&
    request.method !== "HEAD";
  let upstream: Response;
  try {
    upstream = await fetch(
      serverEnv().appApiUrl + "/api/v1/" + resource + request.nextUrl.search,
      {
        method: request.method,
        headers,
        cache: "no-store",
        redirect: "manual",
        signal: AbortSignal.any([request.signal, AbortSignal.timeout(285_000)]),
        ...(hasBody ? { body: request.body, duplex: "half" } : {}),
      } as RequestInit,
    );
  } catch {
    return Response.json(
      {
        error: {
          code: "upstream_unavailable",
          message: "The McPort API did not return a response.",
          recovery: "Check activity before retrying a write.",
          outcome_unknown: hasBody,
        },
      },
      { status: 502, headers: cors },
    );
  }
  const result = relayResponseHeaders(upstream.headers);
  for (const [key, value] of Object.entries(cors)) result.set(key, value);
  return new Response(upstream.body, {
    status: upstream.status,
    headers: result,
  });
}
export {
  handle as GET,
  handle as HEAD,
  handle as POST,
  handle as PUT,
  handle as PATCH,
  handle as DELETE,
};
