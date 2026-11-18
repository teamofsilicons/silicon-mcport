import { createHash, randomBytes, randomUUID } from "node:crypto";
import { expect, test } from "@playwright/test";
import {
  ACCOUNTS,
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
    // Load a real document from the MCP fixture's separate loopback origin.
    // Browser network policy assigns synthetic documents an unknown address space.
    await page.goto("http://127.0.0.1:4242/health");
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

test("an Accounts Interface grant authorizes McPort and stops working on revocation", async ({
  browser,
}) => {
  const id = process.env.E2E_INTERFACE_ID;
  test.skip(
    !id,
    "A registered local public Interface with mcport is required.",
  );
  const redirect =
    process.env.E2E_INTERFACE_CALLBACK ?? "http://127.0.0.1:4242/finish";
  const context = await accountsSession(browser, "carbon");
  const request = context.request;
  const auth = ACCOUNTS + "/v1/interface-auth";
  const state = randomUUID();
  const verifier = randomBytes(48).toString("base64url");
  const headers = { Origin: ACCOUNTS };
  const check = (access: string) =>
    request.get(BASE + "/interface/api/v1/me", {
      headers: { Authorization: "Bearer " + access },
    });
  let refresh: string | undefined;
  let access: string | undefined;
  try {
    const flow = await request.post(auth + "/flows", {
      headers,
      data: {
        interface_id: id,
        redirect_uri: redirect,
        state,
        code_challenge_method: "S256",
        code_challenge: createHash("sha256")
          .update(verifier)
          .digest("base64url"),
      },
    });
    expect(flow.status()).toBe(200);
    const review = await flow.json();
    expect(
      review.apps.some((app: { app_id: string }) => app.app_id === "mcport"),
    ).toBe(true);
    const approved = await request.post(
      auth + "/flows/" + review.flow_id + "/decision",
      { headers, data: { approve: true } },
    );
    expect(approved.status()).toBe(200);
    const callback = new URL((await approved.json()).redirect_to);
    expect(callback.origin + callback.pathname).toBe(redirect);
    expect(callback.searchParams.get("state")).toBe(state);
    const grant = await request.post(auth + "/token", {
      headers: { "Idempotency-Key": randomUUID() },
      data: {
        interface_id: id,
        grant_type: "authorization_code",
        redirect_uri: redirect,
        code: callback.searchParams.get("code"),
        code_verifier: verifier,
      },
    });
    expect(grant.status()).toBe(200);
    const initial = await grant.json();
    refresh = initial.refresh_token;
    expect(
      (await (await check(initial.apps.mcport.access_token)).json()).data.id,
    ).toBe(runAccounts().carbon.id);
    const rotate = await request.post(auth + "/token", {
      headers: { "Idempotency-Key": randomUUID() },
      data: {
        interface_id: id,
        grant_type: "refresh_token",
        refresh_token: refresh,
      },
    });
    expect(rotate.status()).toBe(200);
    const rotated = await rotate.json();
    refresh = rotated.refresh_token;
    access = rotated.apps.mcport.access_token;
    expect((await check(access!)).status()).toBe(200);
  } finally {
    try {
      if (refresh) {
        const revoke = await request.post(auth + "/revoke", {
          data: { interface_id: id, refresh_token: refresh },
        });
        expect(revoke.status()).toBe(200);
        if (access) expect((await check(access)).status()).toBe(401);
      }
    } finally {
      await context.close();
    }
  }
});
