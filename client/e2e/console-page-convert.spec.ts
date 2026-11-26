import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";

// Convert on the helper's own page, for a game in asset packs made into a .pkg: the package holds
// the plain files unless that is turned off, the AMPR emulator can be left out, and a game whose
// emulator can't stay beside plain files (the console downloads no other) holds Convert until it
// is left out. A real browser with the PS5's user agent, the helper's API stood in for; what
// reaches /api/convert/start is what the helper's route reads. Needs `npm run build:console`.

const PS5_AGENT =
  "Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.4 Safari/605.1.15";
const ORIGIN = "http://console.test";
const GAME = "/data/homebrew/PPSA01234";

test.use({ userAgent: PS5_AGENT, viewport: { width: 1920, height: 1080 } });

const dir = (name: string) => ({ name, kind: "dir", size: 0, mtime: 0 });
const FOLDERS: Record<string, ReturnType<typeof dir>[]> = {
  "/data": [dir("homebrew")],
  "/data/homebrew": [dir("PPSA01234")],
  [GAME]: [dir("sce_sys")],
};

/** What the converter answers for the game: in zstd packs, 2 GiB packed and 3 GiB as plain files. */
const inspection = (over: Record<string, unknown>) => ({
  path: GAME,
  format: "folder",
  size: 2 * 1024 ** 3,
  files: 40,
  data_bytes: 2 * 1024 ** 3,
  empty_dirs: 0,
  title_id: "PPSA01234",
  content_id: "UP0000-PPSA01234_00-TESTGAME00000000",
  title: "A Packed Game",
  content_version: "01.000.000",
  master_version: "01.00",
  packed: "zstd",
  unpacked_bytes: 3 * 1024 ** 3,
  unpack_keeps_emulator: true,
  has_eboot: true,
  warnings: [],
  zstd_runtime: "0.4.1",
  ...over,
});

/** Opens the page on Convert with the game picked and read; `started` gets each start's body. */
async function openOnGame(page: Page, errors: string[], result: Record<string, unknown>, started: unknown[]) {
  await page.addInitScript(() => localStorage.setItem("ps5upload.console.tourSeen", "1"));
  page.on("pageerror", (e) => errors.push(e.message));
  const html = readFileSync(resolve(process.cwd(), "dist-console/index.html"), "utf8");
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (!url.pathname.startsWith("/api/")) {
      if (url.pathname === "/" || url.pathname.endsWith(".html")) return route.fulfill({ contentType: "text/html", body: html });
      return route.fulfill({ status: 404, body: "" });
    }
    const json = (body: unknown, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    const body = (route.request().postDataJSON() ?? {}) as { path?: string; job?: string };
    const job = (id: string, text: unknown) => ({
      job: id,
      state: "done",
      files_done: 0,
      files_total: 0,
      bytes_done: 0,
      bytes_total: 0,
      message: "",
      result: JSON.stringify(text),
      code: 0,
    });
    switch (url.pathname) {
      case "/api/token":
        return json({ token: "test" });
      case "/api/volumes":
        return json({
          volumes: [{ path: "/data", mount_from: "/dev/ssd0.user", fs_type: "ufs", total_bytes: 800 * 1024 ** 3, free_bytes: 2.5 * 1024 ** 3, writable: true }],
        });
      case "/api/fs/list": {
        const entries = FOLDERS[body.path ?? ""];
        return entries ? json({ entries, path: body.path, truncated: false }) : json({ error: "not_found" }, 404);
      }
      case "/api/fs/stat":
        return json({ missing: true });
      case "/api/convert/inspect":
        return json({ job: "inspect1" });
      case "/api/convert/start":
        started.push(body);
        return json({ job: "build1" });
      case "/api/jobs/status":
        return json(body.job === "inspect1" ? job("inspect1", result) : job("build1", { path: `${GAME}.pkg` }));
    }
    return json({});
  });
  await page.goto(`${ORIGIN}/`);
  await page.locator('[data-rail-id="convert"]').click({ timeout: 20_000 });
  await page.locator('[data-action="convert-pick-source"]').click();
  await page.locator('[data-pick="/data"]').click();
  await page.locator('[data-pick="homebrew"]').click();
  await page.locator('[data-pick="PPSA01234"]').click();
  await page.locator("[data-pick-here]").click();
  await expect(page.locator("[data-convert-info]")).toContainText("A Packed Game");
}

const unpack = (page: Page) => page.locator('[data-action="convert-unpack"]');
const leaveOut = (page: Page) => page.locator('[data-action="convert-leave-out-emulator"]');
const go = (page: Page) => page.locator('[data-action="convert-go"]');

test("a .pkg of a packed game unpacks its asset packs, and can leave the emulator out", async ({ page }) => {
  const errors: string[] = [];
  const started: unknown[] = [];
  await openOnGame(page, errors, inspection({}), started);
  // Nothing of it with an image: the game is written as it is.
  await expect(unpack(page)).toHaveCount(0);

  await page.locator('[data-action="convert-format-pkg"]').click();
  await expect(unpack(page)).toHaveAttribute("aria-checked", "true");
  await expect(unpack(page)).toContainText("Unpack the asset packs (plain files again)");
  await expect(page.locator("[data-convert-unpack]")).toContainText("This game is in zstd asset packs.");
  await expect(page.locator("[data-convert-unpack]")).toContainText("(3.00 GiB)");
  await expect(leaveOut(page)).toHaveAttribute("aria-checked", "false");
  await expect(leaveOut(page)).toContainText("Leave out the AMPR emulator");
  await expect(page.locator("[data-convert-blocked]")).toHaveCount(0);
  // 2.5 GiB free: the packs (2 GiB) would fit, the plain files (3 GiB) only if compression helps.
  await expect(page.locator("[data-convert-space]")).toBeVisible();

  await go(page).click();
  await expect(page.locator('[data-convert-end="info"]')).toContainText(`${GAME}.pkg`);
  expect(started).toEqual([
    { path: GAME, dest: `${GAME}.pkg`, format: "pkg", zstd: false, pkg_level: "fast", unpack_packs: true, verify: "fast" },
  ]);

  // The emulator left out goes with it.
  await leaveOut(page).click();
  await expect(leaveOut(page)).toHaveAttribute("aria-checked", "true");
  await go(page).click();
  await expect.poll(() => started.length).toBe(2);
  expect(started[1]).toMatchObject({ unpack_packs: true, leave_out_ampr_emu: true });

  // Off: the packs go in as they are, there is no emulator switch, and neither word is sent.
  await unpack(page).click();
  await expect(unpack(page)).toHaveAttribute("aria-checked", "false");
  await expect(leaveOut(page)).toHaveCount(0);
  await expect(page.locator("[data-convert-space]")).toHaveCount(0);
  await go(page).click();
  await expect.poll(() => started.length).toBe(3);
  expect(started[2]).not.toHaveProperty("unpack_packs");
  expect(started[2]).not.toHaveProperty("leave_out_ampr_emu");
  expect(errors).toEqual([]);
});

test("a game whose emulator can't stay with plain files holds Convert until it is left out", async ({ page }) => {
  const errors: string[] = [];
  const started: unknown[] = [];
  await openOnGame(page, errors, inspection({ unpack_keeps_emulator: false }), started);
  await page.locator('[data-action="convert-format-pkg"]').click();

  const blocked = page.locator('[data-convert-blocked="emulator"]');
  await expect(blocked).toContainText("has no emulator that works with plain files");
  await expect(blocked).toContainText("Put the AMPR emulator on this PS5 from the app");
  await expect(go(page)).toBeDisabled();

  await leaveOut(page).click();
  await expect(blocked).toHaveCount(0);
  await expect(go(page)).toBeEnabled();
  await go(page).click();
  await expect.poll(() => started.length).toBe(1);
  expect(started[0]).toMatchObject({ format: "pkg", unpack_packs: true, leave_out_ampr_emu: true });
  expect(errors).toEqual([]);
});

test("with the AMPR emulator put on this PS5 from the app, the same game keeps an emulator", async ({ page }) => {
  const errors: string[] = [];
  const started: unknown[] = [];
  // The helper's answer when ampr_emu is on the console: the emulator can stay, and which.
  await openOnGame(page, errors, inspection({ unpack_keeps_emulator: true, ampr_emu: "0.4.2.1" }), started);
  await page.locator('[data-action="convert-format-pkg"]').click();
  await expect(leaveOut(page)).toHaveAttribute("aria-checked", "false");
  await expect(page.locator("[data-convert-blocked]")).toHaveCount(0);
  await go(page).click();
  await expect.poll(() => started.length).toBe(1);
  expect(started[0]).toMatchObject({ format: "pkg", unpack_packs: true });
  expect(started[0]).not.toHaveProperty("leave_out_ampr_emu");
  expect(errors).toEqual([]);
});

test("a game that is not in packs, or read by an older helper, has no unpack switch", async ({ page }) => {
  const errors: string[] = [];
  const started: unknown[] = [];
  // An older helper's answer for a packed game: no plain size, so nothing to offer.
  await openOnGame(page, errors, inspection({ unpacked_bytes: undefined, unpack_keeps_emulator: undefined }), started);
  await page.locator('[data-action="convert-format-pkg"]').click();
  await expect(page.locator("[data-convert-pkg]")).toBeVisible();
  await expect(unpack(page)).toHaveCount(0);
  await go(page).click();
  await expect.poll(() => started.length).toBe(1);
  expect(started[0]).toEqual({ path: GAME, dest: `${GAME}.pkg`, format: "pkg", zstd: false, pkg_level: "fast", verify: "fast" });
  expect(errors).toEqual([]);
});
