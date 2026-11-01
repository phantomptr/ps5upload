import { expect, test, type Route } from "@playwright/test";

// Save Data around what people do with saves (#612): Back up now and the "Backed up …" line,
// Restore… with Restore to another user, Copy to › Another user on this PS5, and Open with
// readable part names (the console's own sdimg_sce_bu_* copies never offered).

const HOST = "192.168.1.100";
const T = "PPSA01650";
const ANN = 0x10000000;
const BEN = 0x20000000;
const CY = 0x30000000;
const folder = (u: number) => `/user/home/${u.toString(16)}/savedata_prospero/${T}`;
const BACKUP = `${T}_user10000000_2026-10-30_120000.zip`;

function mockEngine() {
  const calls = { copy: [] as unknown[], restoreTo: [] as unknown[], now: [] as unknown[], open: [] as unknown[] };
  let backedUp = false;
  const handler = async (route: Route) => {
    const req = route.request();
    const url = new URL(req.url());
    const json = (b: unknown, status = 200) =>
      route.fulfill({ status, contentType: "application/json", body: JSON.stringify(b) });
    switch (url.pathname) {
      case "/api/version":
        return json({ version: "10.6.0", caps: {} });
      case "/api/ps5/status":
        return json({ ok: true });
      case "/api/profile/info":
        return json({
          ok: true,
          uid: ANN,
          uid_hex: "10000000",
          username: "Ann",
          users: [
            { uid: ANN, uid_hex: "10000000", username: "Ann" },
            { uid: BEN, uid_hex: "20000000", username: "Ben" },
            { uid: CY, uid_hex: "30000000", username: "Cy" },
          ],
          slots: [],
        });
      case "/api/ps5/saves/list":
        return json({
          saves: [
            { title_id: T, user_id: ANN, path: folder(ANN), size: 8192, mtime: 1761700000, kind: "ps5" },
            { title_id: T, user_id: BEN, path: folder(BEN), size: 4096, mtime: 1761600000, kind: "ps5" },
            { title_id: "PPSA00001", user_id: CY, path: folder(CY).replace(T, "PPSA00001"), size: 1, mtime: 1, kind: "ps5" },
          ],
        });
      case "/api/ps5/list-dir":
        return json({
          entries: ["sdimg_STARBOARD", "sdimg_sce_sdmemory", "sdimg_sce_bu_sce_sdmemory", "sdimg_sce_bu_STARBOARD", "STARBOARD.bin"].map(
            (name) => ({ name, kind: "file", size: 1, mtime: 1 }),
          ),
          truncated: false,
        });
      case "/api/ps5/saves/open": {
        const body = req.postDataJSON() as { image: string };
        if (!body.image) return json({ mounted: false, image: "", mount_point: "", kind: "" });
        calls.open.push(body.image);
        return json({ mounted: true, image: body.image, mount_point: "/data/ps5upload/save_mount", kind: "ps5" });
      }
      case "/api/save-backup":
        return json({
          settings: { enabled: true, dir: "/backups", keep: 5, hosts: [HOST] },
          status: { running: false, last_at: 0, backed_up: 0, failed: 0, last_error: null },
        });
      case "/api/save-backup/server":
        return json({
          settings: { mode: "off", target: "", every_hours: 24, keep: 5, hosts: [], users: [] },
          status: { running: false, last_at: 0, last_trigger: "", backed_up: 0, verified: 0, failed: 0, last_error: null, last_verified: null, last_check_at: 0 },
          next_at: null,
        });
      case "/api/save-backup/latest":
        return json({ latest: backedUp ? [{ title_id: T, user_id: ANN, stamp: "2026-10-30_120000" }] : [] });
      case "/api/save-backup/now":
        calls.now.push(req.postDataJSON());
        backedUp = true;
        return json({ ok: true, file: BACKUP });
      case "/api/save-backup/list":
        return json({
          backups:
            url.searchParams.get("title_id") === T && backedUp
              ? [{ file: BACKUP, title_id: T, user_id: ANN, stamp: "2026-10-30_120000", size: 2048 }]
              : [],
        });
      case "/api/save-backup/copy":
        calls.copy.push(req.postDataJSON());
        return json({ ok: true, from: folder(ANN), to: folder(BEN), parts: ["sdimg_STARBOARD", "sdimg_sce_sdmemory"] });
      case "/api/save-backup/restore-to-user":
        calls.restoreTo.push(req.postDataJSON());
        return json({ ok: true, from: BACKUP, to: folder(BEN), parts: ["sdimg_STARBOARD"] });
      case "/api/save-backup/switch":
        return json({ running: null, saves: [] });
      case "/api/save-backup/ps2":
        return json({ saves: [] });
      default:
        return json({});
    }
  };
  return { calls, handler };
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript((h) => window.localStorage.setItem("ps5upload.host", h), HOST);
});

test("Copy to another user, Back up now, then Restore to another user", async ({ page }) => {
  const { calls, handler } = mockEngine();
  await page.route((url) => url.pathname.startsWith("/api/"), handler);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  // All users, so Ann's and Ben's saves both show.
  await page.getByLabel("Whose saves").selectOption("all", { timeout: 30_000 });
  await expect(page.getByTestId("saves-how")).toBeVisible();
  await expect(page.getByTestId("saves-copy-tip")).toBeVisible();

  const ann = page.getByTestId("save-row").filter({ hasText: "Ann" });
  await expect(ann.getByTestId("save-backup-status")).toHaveText("Not backed up yet");

  // Copy to › Another user on this PS5: Ben has a save and can be picked, Cy can't.
  await ann.getByRole("button", { name: "Move or share this save" }).click();
  await page.getByTestId("save-copy-to-user").click();
  const picker = page.getByTestId("copy-user-picker");
  await expect(picker.getByTestId("copy-user-target")).toHaveCount(2);
  await expect(picker.getByText("Start PPSA01650 once as Cy and save, then come back.")).toBeVisible();
  await expect(picker.locator('[data-has-save="false"]')).toBeDisabled();
  await picker.locator('[data-has-save="true"]').click();
  await expect(page.getByTestId("copy-user-confirm")).toContainText("Replace Ben's save of PPSA01650 with Ann's?");
  await page.getByTestId("copy-user-replace").click();
  await expect(page.getByTestId("copy-user-done")).toContainText("Start the game as Ben to load it.");
  expect(calls.copy).toEqual([{ addr: HOST, title_id: T, from_user: ANN, to_user: BEN }]);
  await page.getByRole("button", { name: "Close", exact: true }).last().click();

  // Back up now → the line says so.
  await ann.getByTestId("save-back-up-now").click();
  await expect.poll(() => calls.now.length).toBe(1);
  expect(calls.now[0]).toEqual({ addr: HOST, title_id: T, user_id: ANN });
  await expect(ann.getByTestId("save-backup-status")).toHaveAttribute("data-backed-up", "true");

  // Restore… lists that backup as one row, with Restore to another user.
  await ann.getByTestId("save-restore-pick").click();
  const dialog = page.getByTestId("save-restore-dialog");
  await expect(dialog.getByText(/2026-10-30 12:00 UTC/)).toHaveCount(1);
  await dialog.getByTestId("save-backup-restore-to-user").click();
  await page.locator('[data-testid="copy-user-target"][data-has-save="true"]').click();
  await page.getByTestId("copy-user-replace").click();
  await expect(page.getByTestId("copy-user-done")).toContainText("now has the backup from 2026-10-30 12:00 UTC");
  expect(calls.restoreTo).toEqual([{ addr: HOST, title_id: T, file: BACKUP, to_user: BEN, server: false }]);
});

test("Open names the save's parts and never offers the console's backup copies", async ({ page }) => {
  const { calls, handler } = mockEngine();
  await page.route((url) => url.pathname.startsWith("/api/"), handler);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  const ann = page.getByTestId("save-row").filter({ hasText: "Ann" });
  await ann.getByTestId("save-open").click({ timeout: 30_000 });
  const picker = page.getByTestId("save-open-picker");
  await expect(picker.getByRole("button")).toHaveCount(2);
  await expect(picker).not.toContainText("sce_bu");
  await expect(picker.getByRole("button", { name: /main save/ })).toBeVisible();
  // Picking one opens it (it did nothing before).
  await picker.getByRole("button", { name: /STARBOARD/ }).click();
  await expect.poll(() => calls.open).toEqual([`${folder(ANN)}/sdimg_STARBOARD`]);
  await expect(page.getByTestId("save-open-banner")).toBeVisible();
});

test("a single-user console offers no Copy to another user", async ({ page }) => {
  const { handler } = mockEngine();
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === "/api/profile/info")
        return route.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true, uid: ANN, users: [{ uid: ANN, uid_hex: "10000000", username: "Ann" }], slots: [] }) });
      if (url.pathname === "/api/ps5/saves/list")
        return route.fulfill({ contentType: "application/json", body: JSON.stringify({ saves: [{ title_id: T, user_id: ANN, path: folder(ANN), size: 1, mtime: 1, kind: "ps5" }] }) });
      return handler(route);
    },
  );
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Move or share this save" }).click({ timeout: 30_000 });
  await expect(page.getByTestId("save-share")).toBeVisible();
  await expect(page.getByTestId("save-copy-to-user")).toHaveCount(0);
  await expect(page.getByTestId("saves-copy-tip")).toHaveCount(0);
});
