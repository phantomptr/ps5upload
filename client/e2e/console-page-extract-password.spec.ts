import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// Files › Extract of an archive with a password, on the helper's own page, the helper's API stood
// in for. Needs `npm run build:console` first. The extract runs on while other dialogs are opened;
// its password question replaced whatever was open (an editor with its unsaved text). It now shows
// over that dialog, which stays.

const ORIGIN = "http://console.test";
const G = 1024 ** 3;
const entry = (name: string, kind: "file" | "dir") => ({ name, kind, size: kind === "file" ? 1000 : 0, mtime: 1764000000 });

test.use({ viewport: { width: 1280, height: 800 } });

test("an extract's password question shows over the open dialog, which stays", async ({ page }) => {
  const errors: string[] = [];
  let ended = false;
  await page.addInitScript(() => localStorage.setItem("ps5upload.console.tourSeen", "1"));
  page.on("pageerror", (e) => errors.push(e.message));
  const html = readFileSync(resolve(process.cwd(), "dist-console/index.html"), "utf8");
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/" || url.pathname.endsWith(".html")) return route.fulfill({ contentType: "text/html", body: html });
    if (!url.pathname.startsWith("/api/")) return route.fulfill({ status: 404, body: "" });
    const json = (body: unknown, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    switch (url.pathname) {
      case "/api/token":
        return json({ token: "test" });
      case "/api/volumes":
        return json({ volumes: [{ path: "/data", mount_from: "/dev/ssd0.user", fs_type: "exfatfs", total_bytes: 800 * G, free_bytes: 100 * G, writable: true }] });
      case "/api/fs/list":
        return json({ entries: [entry("games", "dir"), entry("locked.zip", "file")], path: "/data", truncated: false });
      case "/api/fs/stat":
        return json({ error: "not_found" }, 404);
      case "/api/fs/extract/info":
        return json({ name: "locked", dest: "/data/locked", size: 1000, volumes: 1, free: 100 * G, exists: false, kind: "zip" });
      case "/api/fs/extract":
        return json({ job: "e".repeat(32) });
      case "/api/jobs/status":
        return json(
          ended
            ? { job: "e".repeat(32), state: "failed", bytes_done: 0, bytes_total: 1000, message: "fs_extract_password_required" }
            : { job: "e".repeat(32), state: "running", bytes_done: 10, bytes_total: 1000, message: "" },
        );
      default:
        return json({});
    }
  });
  await page.goto(`${ORIGIN}/`);
  await page.locator('[data-rail-id="files"]').click({ timeout: 20_000 });
  await page.locator('[data-drive="/data"]').click();
  await page.locator('[data-entry="locked.zip"] .cw-icon-button').click();
  await page.locator('[data-sheet="extract"]').click();
  await page.locator('[data-action="extract-go"]').click();
  // while it runs, another dialog: New folder
  await page.locator('[data-action="mkdir"]').click();
  const prompt = page.locator('[role="dialog"]');
  await expect(prompt).toHaveCount(1);
  const promptLabel = await prompt.getAttribute("aria-label");
  ended = true;
  // the question comes over the New folder prompt, which stays under it
  await expect(page.locator('[role="dialog"] input[type="password"]')).toBeVisible({ timeout: 10_000 });
  await expect(prompt).toHaveCount(2);
  expect(await prompt.first().getAttribute("aria-label")).toBe(promptLabel);
  expect(await prompt.last().getAttribute("aria-label")).toBe("Password for “locked.zip”");
  // ○ (Escape on a keyboard) closes the question only
  await page.keyboard.press("Escape");
  await expect(prompt).toHaveCount(1);
  expect(await prompt.getAttribute("aria-label")).toBe(promptLabel);
  expect(errors).toEqual([]);
});
