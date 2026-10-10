/** The public files built from lib/app.config.ts: robots.txt, the sitemap and the llms.txt stub. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { llmsTxt, robotsTxt, sitemapXml } from "../lib/agent/files";
import { markSvg } from "../lib/brand-svg";

const ORIGIN = "https://mcport.example";

test("robots.txt opens the public pages and keeps the workspace, sign-in and the proxy out", () => {
  const text = robotsTxt(ORIGIN);
  assert.match(text, /^# https:\/\/mcport\.example: MCPort\./);
  for (const line of ["User-agent: ClaudeBot", "User-agent: *", "Allow: /", "Allow: /llms.txt", "Disallow: /api/", "Disallow: /auth/", "Disallow: /sign-in", "Disallow: /connections", "Disallow: /settings", `Sitemap: ${ORIGIN}/sitemap.xml`]) {
    assert.ok(text.split("\n").includes(line), line);
  }
});

test("the sitemap lists the public pages with their date", () => {
  const xml = sitemapXml(ORIGIN, "2026-10-10");
  assert.match(xml, /^<\?xml version="1\.0" encoding="UTF-8"\?>/);
  assert.ok(xml.includes(`<url><loc>${ORIGIN}/</loc><lastmod>2026-10-10</lastmod></url>`));
  assert.ok(xml.includes(`<loc>${ORIGIN}/llms.txt</loc>`));
});

test("llms.txt says what the app is, how a Silicon installs it and signs in, and where the docs are", () => {
  const text = llmsTxt(ORIGIN);
  assert.match(text, /^# MCPort\n\n> One place for your MCP connections\.\n/);
  assert.ok(text.includes("`silicon-apps install mcport`"));
  assert.ok(text.includes("`silicon-accounts login --app mcport -q | mcport login --slt-stdin`"));
  assert.ok(text.includes("- [Docs](https://github.com/teamofsilicons/silicon-mcport/tree/main/docs)"));
  for (const word of ["AI agent", "human", "user account", "organization", " org ", "Honeycomb", "IAM"]) assert.ok(!text.includes(word), `never "${word}"`);
});

test("the mark is the configured glyph, white on a squircle of the app's colour", () => {
  const svg = markSvg();
  assert.match(svg, /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg" viewBox="0 0 64 64"><path fill="#1F5FB8" d="M32 0c19\.6/);
  assert.ok(svg.includes('stroke="#FFFFFF"'));
  assert.ok(svg.includes('<path d="M7 2v5"/>'));
  assert.match(markSvg({ shape: "square" }), /<rect width="64" height="64" fill="#1F5FB8"\/>/);
});
