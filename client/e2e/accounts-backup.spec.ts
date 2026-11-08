import { expect, test } from "@playwright/test";
import { readFile } from "node:fs/promises";

// Accounts › Back up user information downloads a text file with every user, their account ID
// in every form, their sign-in state, and how to give a new user the old account ID.
test("Accounts backs up the user information as a text file", async ({ page }) => {
  const npAsked: unknown[] = [];
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
    const path = new URL(route.request().url()).pathname;
    let body: unknown = {};
    if (path === "/api/version") body = { version: "9.5.3", caps: {} };
    if (path === "/api/ps5/status") body = { ok: true };
    if (path === "/api/profile/info")
      body = {
        ok: true,
        uid: 268435456,
        uid_hex: "0x10000000",
        username: "Alice",
        users: [
          { uid: 268435456, uid_hex: "0x10000000", username: "Alice" },
          { uid: 268435457, uid_hex: "0x10000001", username: "Bob" },
        ],
        slots: [
          { slot: 1, name: "Alice", type: "", flags: 4098, id: "0x1122334455667788", activated: true, offline_activated: true },
        ],
      };
    if (path === "/api/profile/avatar/current") body = { data_url: null };
    if (path === "/api/profile/np/state") {
      npAsked.push(route.request().postDataJSON());
      body = {
        ok: true, slot: 1, uid: 268435456, backup: false, backup_slot: 0, online_id: "alice_ps",
        dob: "1983-04-12", auth_dat: true, config_dat: true, sub_account: 0, offline_adult: 1, age: 43, login_flag: 1,
      };
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto("/accounts", { waitUntil: "domcontentloaded" });
  const button = page.getByTestId("accounts-backup");
  await expect(button).toBeVisible({ timeout: 30_000 });
  // The page reads sign-in state on its own; count only what the backup asks. Its own read can
  // land after the button shows on a busy runner, so it is waited for first.
  await expect.poll(() => npAsked.length, { timeout: 30_000 }).toBeGreaterThan(0);
  npAsked.length = 0;
  const download = page.waitForEvent("download");
  await button.click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^ps5upload-users-.+-\d{4}-\d{2}-\d{2}\.txt$/);
  const text = await readFile((await file.path())!, "utf8");
  expect(text).toContain("1. Alice   [signed in now]");
  expect(text).toContain("Account ID (hex):    0x1122334455667788");
  expect(text).toContain("Account ID (decimal): 1234605616436508552");
  expect(text).toContain("Account ID (base64): iHdmVUQzIhE=");
  expect(text).toContain("Online ID:           alice_ps");
  expect(text).toContain("2. Bob");
  expect(text).toContain("RESTORING");
  // Only the user with an account slot is asked for its sign-in state.
  expect(npAsked).toEqual([expect.objectContaining({ slot: 1, uid: 268435456 })]);
  await expect(page.getByTestId("accounts-backup-done")).toContainText("ps5upload-users-");
});
