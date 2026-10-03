import { describe, expect, it } from "vitest";
import { safeResourceLink } from "../src/McpResult";
describe("MCP result resource links", () => {
  it("allows ordinary explicit public HTTP resource links", () => {
    expect(safeResourceLink("https://example.com/results/a?q=1")).toBe(
      "https://example.com/results/a?q=1",
    );
  });
  it.each([
    "javascript:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "file:///Users/private/result.pdf",
    "https://user:password@example.com/file",
    "http://localhost:3845/asset",
    "http://127.8.2.3:3845/asset",
    "http://192.168.1.5/asset",
    "http://169.254.169.254/latest/meta-data",
    "http://[::1]/asset",
    "http://[fd00::1]/asset",
  ])("does not turn %s into a caller-side link", (value) => {
    expect(safeResourceLink(value)).toBeNull();
  });
});
