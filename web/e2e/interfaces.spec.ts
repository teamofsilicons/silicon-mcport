import { expect, test } from "@playwright/test";
import {
  BASE,
  accountsSession,
  runAccounts,
  signIn,
  storedSession,
} from "./support";

test("a standalone browser can use McPort with its own app bearer", async ({
  browser,
}) => {
  const primary = await accountsSession(browser, "carbon");
  const standalone = await browser.newContext();
  try {
    await signIn(await primary.newPage(), runAccounts().carbon.email);
    const { session } = await storedSession(primary);
    const page = await standalone.newPage();
    await page.route("http://interface.example.test/", (route) =>
      route.fulfill({
        contentType: "text/html",
        body: "<title>Independent interface</title>",
      }),
    );
    await page.goto("http://interface.example.test/");
    const result = await page.evaluate(
      async ({ base, token }) => {
        const url = base + "/interface/api/v1";
        const headers = {
          Authorization: "Bearer " + token,
          "Content-Type": "application/json",
        };
        const send = (path: string, method = "GET", body?: object) =>
          fetch(url + path, {
            method,
            headers,
            credentials: "omit",
            body: JSON.stringify(body),
          });
        const unauthorized = await fetch(url + "/me", { credentials: "omit" });
        const me = await (await send("/me")).json();
        const organizations = await (await send("/organizations")).json();
        const added = await send("/directory", "POST", {
          name: "Standalone browser",
          category: "Test",
        });
        const entry = (await added.json()).data;
        if (!entry?.id) throw new Error("The directory entry was not created");
        try {
          const read = await (await send("/directory/" + entry.id)).json();
          return {
            unauthorized: unauthorized.status,
            id: me.data.id,
            teams: organizations.data.share_targets,
            entryName: read.data.name,
          };
        } finally {
          const removed = await send("/directory/" + entry.id, "DELETE");
          if (!removed.ok)
            throw new Error("The directory entry could not be removed");
        }
      },
      { base: BASE, token: session.at },
    );
    expect(result).toEqual({
      unauthorized: 401,
      id: runAccounts().carbon.id,
      teams: [],
      entryName: "Standalone browser",
    });
  } finally {
    await standalone.close();
    await primary.close();
  }
});

test("the people picker preserves a saved contact's picture and trust after keyboard selection", async ({
  browser,
}) => {
  const context = await accountsSession(browser, "carbon");
  try {
    const page = await context.newPage();
    const contacts = [
      {
        uuid: "2bdd54eb-43bc-44bd-b28d-3f3ca59edad9",
        id: "si:design",
        kind: "silicon",
        display_name: "Design Studio",
        trust: 200,
        photo_id: null,
        pfp_url: "/icon",
      },
      {
        uuid: "e06b2942-6e9d-465c-8834-b586a69ea96b",
        id: "c:designer",
        kind: "carbon",
        display_name: "Designer",
        trust: 1,
        photo_id: null,
        pfp_url: null,
      },
    ];
    await page.route("**/api/api/v1/people", (route) =>
      route.fulfill({ json: { data: { connected: true, items: contacts } } }),
    );
    await signIn(page, runAccounts().carbon.email, "/settings");
    const field = page.getByRole("combobox", { name: "Contact account" });
    await field.fill("dsg");
    const suggestions = page.getByRole("listbox", {
      name: "Contact account suggestions",
    });
    await expect(suggestions.getByRole("option")).toHaveCount(2);
    await expect(suggestions.getByRole("option").first()).toContainText(
      "Design Studio",
    );
    await field.press("ArrowDown");
    await field.press("Enter");
    await expect(field).toHaveValue("si:design");
    await expect(page.getByLabel("Saved name", { exact: true })).toHaveValue(
      "Design Studio",
    );
    await expect(page.getByLabel("Trust", { exact: true })).toHaveValue("200");
    await expect(page.locator(".person-preview img")).toHaveAttribute(
      "src",
      "/icon",
    );
  } finally {
    await context.close();
  }
});
