import { cp, mkdir, rm, stat, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function deploymentConfig(value) {
  let backend;
  try {
    backend = new URL(value);
  } catch {
    throw new Error(
      "Set MCPORT_BACKEND_ORIGIN to the chosen HTTPS backend origin before building Vercel output.",
    );
  }
  if (
    backend.protocol !== "https:" ||
    backend.pathname !== "/" ||
    backend.search ||
    backend.hash ||
    backend.username ||
    backend.password
  ) {
    throw new Error(
      "MCPORT_BACKEND_ORIGIN must be an exact HTTPS origin without credentials, a path, query or fragment.",
    );
  }
  return {
    version: 3,
    routes: [
      {
        src: "/(.*)",
        headers: {
          "X-Content-Type-Options": "nosniff",
          "Referrer-Policy": "no-referrer",
          "X-Frame-Options": "DENY",
          "Cache-Control": "no-store",
        },
        continue: true,
      },
      // Keep the backend's stricter attachment CSP on /api result downloads.
      {
        src: "/((?!api(?:/|$)).*)",
        headers: {
          "Content-Security-Policy":
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src 'self' https://fonts.gstatic.com; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        },
        continue: true,
      },
      // API routing precedes filesystem/SPA handling and never depends on a request-supplied host.
      {
        src: "/(api(?:/.*)?)",
        dest: `${backend.origin}/$1`,
        headers: {
          "Cache-Control": "private, no-store",
          "CDN-Cache-Control": "no-store",
          "Vercel-CDN-Cache-Control": "no-store",
        },
      },
      {
        src: "/assets/(.*)",
        headers: { "Cache-Control": "public, max-age=31536000, immutable" },
        continue: true,
      },
      {
        src: "/index.html",
        headers: { "Cache-Control": "no-store" },
        continue: true,
      },
      { handle: "filesystem" },
      // Missing chunks are 404s, never an HTML response masquerading as JavaScript.
      {
        src: "/assets/(.*)",
        status: 404,
        headers: { "Cache-Control": "no-store" },
      },
      {
        src: "/(.*)",
        methods: ["GET", "HEAD"],
        dest: "/index.html",
        headers: { "Cache-Control": "no-store" },
      },
    ],
  };
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const config = deploymentConfig(process.env.MCPORT_BACKEND_ORIGIN);
  if (!process.argv.includes("--check")) {
    const root = fileURLToPath(new URL("../", import.meta.url));
    const output = resolve(root, ".vercel/output");
    await stat(resolve(root, "dist/index.html"));
    await rm(output, { recursive: true, force: true });
    await mkdir(output, { recursive: true });
    await cp(resolve(root, "dist"), resolve(output, "static"), {
      recursive: true,
    });
    await writeFile(
      resolve(output, "config.json"),
      `${JSON.stringify(config, null, 2)}\n`,
    );
    console.info(
      "Prepared static Vercel output with same-origin API proxying. No deployment was created.",
    );
  }
}
