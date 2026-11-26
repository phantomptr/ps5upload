import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// Files › New file on the helper's own page, the helper's API stood in for. Needs
// `npm run build:console` first. The prompt closes at once; if another folder was opened before the
// console answered, the editor opened the file of that name in the folder shown now.

const ORIGIN = "http://console.test";
const G = 1024 ** 3;
const entry = (name: string, kind: "file" | "dir") => ({ name, kind, size: kind === "file" ? 12 : 0, mtime: 1764000000 });
const FOLDERS: Record<string, unknown[]> = {
  "/data": [entry("games", "dir")],
  "/data/games": [entry("notes.txt", "file")],
};

test.use({ viewport: { width: 1280, height: 800 } });

test("New file does not open another folder's file when the folder changed meanwhile", async ({ page }) => {
  const errors: string[] = [];
  const reads: string[] = [];
  let answer: (() => void) | null = null;
  await page.addInitScript(() => localStorage.setItem("ps5upload.console.tourSeen", "1"));
  page.on("pageerror", (e) => errors.push(e.message));
  const html = readFileSync(resolve(process.cwd(), "dist-console/index.html"), "utf8");
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/" || url.pathname.endsWith(".html")) return route.fulfill({ contentType: "text/html", body: html });
    if (!url.pathname.startsWith("/api/")) return route.fulfill({ status: 404, body: "" });
    const json = (body: unknown, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    const body = (() => {
      try {
        return (route.request().postDataJSON() ?? {}) as { path?: string };
      } catch {
        return {};
      }
    })();
    switch (url.pathname) {
      case "/api/token":
        return json({ token: "test" });
      case "/api/volumes":
        return json({ volumes: [{ path: "/data", mount_from: "/dev/ssd0.user", fs_type: "exfatfs", total_bytes: 800 * G, free_bytes: 100 * G, writable: true }] });
      case "/api/fs/list": {
        const p = body.path ?? "";
        return FOLDERS[p] ? json({ entries: FOLDERS[p], path: p, truncated: false }) : json({ error: "not_found" }, 404);
      }
      case "/api/fs/save-text":
        // the console takes its time creating the file
        await new Promise<void>((res) => (answer = res));
        return json({ ok: true, mtime: 1764000001, size: 0 });
      case "/api/fs/read":
      case "/api/fs/stat":
        reads.push(`${url.pathname} ${body.path ?? ""}`);
        return json({ kind: "file", size: 12, mtime: 1764000000 });
      default:
        return json({});
    }
  });
  await page.goto(`${ORIGIN}/`);
  await page.locator('[data-rail-id="files"]').click({ timeout: 20_000 });
  await page.locator('[data-drive="/data"]').click();
  await expect(page.locator('[data-entry="games"]')).toBeVisible();
  await page.locator('[data-action="newfile"]').click();
  await page.locator("form input").first().fill("notes.txt");
  await page.locator("form input").first().press("Enter");
  // before the console answers, another folder with a file of that name is opened
  await page.locator('[data-entry="games"] [data-entry-main]').click();
  await expect(page.locator('[data-entry="notes.txt"]')).toBeVisible();
  await expect.poll(() => answer !== null).toBe(true);
  answer!();
  await page.waitForTimeout(800);
  expect(reads.filter((r) => r.includes("/data/games/notes.txt"))).toEqual([]);
  await expect(page.locator('[role="dialog"]')).toHaveCount(0);
  expect(errors).toEqual([]);
});
