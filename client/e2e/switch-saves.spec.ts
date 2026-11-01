import { expect, test } from "@playwright/test";

// ProsperoEden (Switch emulator) saves in Saves (10.3.0): a group of their own, shown only when the
// console has one, each game per profile with its backups, Back up now and Restore. The engine
// refuses a restore while the emulator runs; the app shows its reason as it is.

const HOST = "192.168.1.100";
const ROOT = "/data/prosperoeden/user/nand/user/save/0000000000000000";
const GAME = "0100F2C0115B6000";
const PROFILE = "1F1E1D1C1B1A19181716151413121110";
const REFUSED = "ProsperoEden (PPSA99008) is running on the PS5. Close it, then restore.";

function mockEngine(switchSaves: unknown[], running: boolean | null) {
  const calls = { snapshot: [] as unknown[], restore: [] as unknown[], lists: [] as string[] };
  const handler = async (route: import("@playwright/test").Route) => {
    const req = route.request();
    const url = new URL(req.url());
    const json = (b: unknown, status = 200) =>
      route.fulfill({ status, contentType: "application/json", body: JSON.stringify(b) });
    switch (url.pathname) {
      case "/api/version":
        return json({ version: "10.3.0", caps: {} });
      case "/api/ps5/status":
        return json({ ok: true });
      case "/api/ps5/saves/list":
        return json({
          saves: [
            { title_id: "PPSA01234", user_id: 0x10000000, path: "/user/home/10000000/savedata_prospero/PPSA01234", size: 4096, mtime: 1761700000, kind: "ps5" },
          ],
        });
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
      case "/api/save-backup/switch":
        expect(url.searchParams.get("addr")).toBe(HOST);
        return json({ running, saves: switchSaves });
      case "/api/save-backup/list": {
        const title = url.searchParams.get("title_id") ?? "";
        calls.lists.push(title);
        if (title !== GAME) return json({ backups: [] });
        return json({
          backups: [
            // The profile's backup; the console's own save has none.
            { file: `${GAME}_user01f1e1d1_2026-10-30_120000.zip`, title_id: GAME, user_id: 0x1f1e1d1, stamp: "2026-10-30_120000", size: 2048 },
          ],
        });
      }
      case "/api/save-backup/snapshot":
        calls.snapshot.push(req.postDataJSON());
        return json({ ok: true, files: [] });
      case "/api/save-backup/restore":
        calls.restore.push(req.postDataJSON());
        return json({ error: REFUSED }, 502);
      default:
        return json({});
    }
  };
  return { calls, handler };
}

test("Saves lists the Switch emulator's saves with their backups", async ({ page }) => {
  const saves = [
    { title_id: GAME, user_id: 0x1f1e1d1, path: `${ROOT}/${PROFILE}/${GAME}`, size: 17, mtime: 1761800000, kind: "switch", running: true },
    { title_id: GAME, user_id: 0, path: `${ROOT}/${"0".repeat(32)}/${GAME}`, size: 3, mtime: 1761700000, kind: "switch", running: true },
  ];
  const { calls, handler } = mockEngine(saves, true);
  await page.addInitScript((h) => window.localStorage.setItem("ps5upload.host", h), HOST);
  await page.route((url) => url.pathname.startsWith("/api/"), handler);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });

  const group = page.getByTestId("switch-saves");
  await expect(group).toBeVisible({ timeout: 30_000 });
  await expect(group.getByRole("heading", { name: "Switch games (ProsperoEden)" })).toBeVisible();
  await expect(group.getByText(GAME, { exact: true })).toBeVisible();
  await expect(group.getByText("Switch", { exact: true })).toBeVisible();
  const rows = group.getByTestId("switch-save");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("Profile 1F1E1D1");
  await expect(rows.nth(1)).toContainText("Console");
  await expect(rows.nth(1)).toContainText("No backups yet.");
  // The emulator runs: the note says so.
  await expect(page.getByTestId("switch-saves-note")).toHaveText(
    "ProsperoEden is running. Close it before restoring.",
  );

  await page.screenshot({
    path: test.info().outputPath("switch-saves.png"),
    fullPage: true,
  });

  // Back up now takes a snapshot of the game.
  await rows.nth(0).getByTestId("switch-save-backup").click();
  await expect.poll(() => calls.snapshot.length).toBe(1);
  expect(calls.snapshot[0]).toEqual({ addr: HOST, title_id: GAME });

  // Restore goes to that profile's save folder, and the engine's refusal shows as it is.
  await rows.nth(0).getByRole("button", { name: "Restore" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Restore" }).click();
  await expect.poll(() => calls.restore.length).toBe(1);
  expect(calls.restore[0]).toEqual({
    addr: HOST,
    title_id: GAME,
    file: `${GAME}_user01f1e1d1_2026-10-30_120000.zip`,
    save_path: `${ROOT}/${PROFILE}/${GAME}`,
    user_id: 0x1f1e1d1,
  });
  await expect(rows.nth(0).getByTestId("save-backup-restore-error")).toHaveText(
    `Couldn't restore the save: ${REFUSED}`,
  );
  await page.screenshot({
    path: test.info().outputPath("switch-saves-refused.png"),
    fullPage: true,
  });
});

test("Saves shows no Switch group when ProsperoEden has no saves", async ({ page }) => {
  const { handler } = mockEngine([], null);
  await page.addInitScript((h) => window.localStorage.setItem("ps5upload.host", h), HOST);
  await page.route((url) => url.pathname.startsWith("/api/"), handler);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  await expect(page.locator('[data-title-id="PPSA01234"]')).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("switch-saves")).toHaveCount(0);
});
