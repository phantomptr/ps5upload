import { expect, test } from "@playwright/test";

// The PS2 emulators' saves in Saves (#602): Crux2 and PS5SX2 each keep their memory cards (user 0)
// and saved states (user 1) as two saves under the emulator's own title ID. A group of their own,
// shown only when the console has one, with backups, Back up now and Restore.

const HOST = "192.168.1.100";
const PS5SX2 = "PPSA99203";
const CRUX2 = "PPSA99249";
const REFUSED = "PS5SX2 (PPSA99203) is running on the PS5. Close it, then restore.";

function mockEngine(ps2Saves: unknown[]) {
  const calls = { snapshot: [] as unknown[], restore: [] as unknown[] };
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
        return json({ running: null, saves: [] });
      case "/api/save-backup/ps2":
        expect(url.searchParams.get("addr")).toBe(HOST);
        return json({ saves: ps2Saves });
      case "/api/save-backup/list": {
        const title = url.searchParams.get("title_id") ?? "";
        if (title !== PS5SX2) return json({ backups: [] });
        return json({
          backups: [
            // The cards' backup; the saved states have none.
            { file: `${PS5SX2}_user00000000_2026-10-30_120000.zip`, title_id: PS5SX2, user_id: 0, stamp: "2026-10-30_120000", size: 2048 },
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

test("Saves lists the PS2 emulators' cards and saved states with their backups", async ({ page }) => {
  const saves = [
    { title_id: PS5SX2, user_id: 1, path: "/data/PCSX2/*.p2s", size: 9_000_000, mtime: 1761800000, kind: "ps2", emulator: "PS5SX2", folder: "/data/PCSX2", running: true },
    { title_id: PS5SX2, user_id: 0, path: "/data/PCSX2/*.ps2", size: 17_301_504, mtime: 1761700000, kind: "ps2", emulator: "PS5SX2", folder: "/data/PCSX2", running: true },
    { title_id: CRUX2, user_id: 0, path: "/data/crux2/memcards", size: 8_650_752, mtime: 1761600000, kind: "ps2", emulator: "Crux2", folder: "/data/crux2/memcards", running: false },
  ];
  const { calls, handler } = mockEngine(saves);
  await page.addInitScript((h) => window.localStorage.setItem("ps5upload.host", h), HOST);
  await page.route((url) => url.pathname.startsWith("/api/"), handler);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });

  const group = page.getByTestId("ps2-saves");
  await expect(group).toBeVisible({ timeout: 30_000 });
  await expect(group.getByRole("heading", { name: "PS2 emulators (Crux2, PS5SX2)" })).toBeVisible();
  const ps5sx2 = group.locator(`[data-title-id="${PS5SX2}"]`);
  const crux2 = group.locator(`[data-title-id="${CRUX2}"]`);
  // One emulator runs, the other does not: each note says so.
  await expect(ps5sx2.getByTestId("ps2-saves-note")).toHaveText(
    "PS5SX2 is running. Close it before restoring.",
  );
  await expect(crux2.getByTestId("ps2-saves-note")).toHaveText("Close Crux2 before restoring.");
  // The cards first, then the saved states.
  const rows = ps5sx2.getByTestId("ps2-save");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("Memory cards");
  await expect(rows.nth(1)).toContainText("Saved states");
  await expect(rows.nth(1)).toContainText("No backups yet.");
  await expect(crux2.getByTestId("ps2-save")).toHaveCount(1);

  await page.screenshot({ path: test.info().outputPath("ps2-saves.png"), fullPage: true });

  // Back up now takes a snapshot of the emulator's saves.
  await rows.nth(0).getByTestId("ps2-save-backup").click();
  await expect.poll(() => calls.snapshot.length).toBe(1);
  expect(calls.snapshot[0]).toEqual({ addr: HOST, title_id: PS5SX2 });

  // Restore goes to the cards' place with the cards' user number; the refusal shows as it is.
  await rows.nth(0).getByRole("button", { name: "Restore" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Restore" }).click();
  await expect.poll(() => calls.restore.length).toBe(1);
  expect(calls.restore[0]).toEqual({
    addr: HOST,
    title_id: PS5SX2,
    file: `${PS5SX2}_user00000000_2026-10-30_120000.zip`,
    save_path: "/data/PCSX2/*.ps2",
    user_id: 0,
  });
  await expect(rows.nth(0).getByTestId("save-backup-restore-error")).toHaveText(
    `Couldn't restore the save: ${REFUSED}`,
  );
});

test("Saves shows no PS2 group when no emulator has saves", async ({ page }) => {
  const { handler } = mockEngine([]);
  await page.addInitScript((h) => window.localStorage.setItem("ps5upload.host", h), HOST);
  await page.route((url) => url.pathname.startsWith("/api/"), handler);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  await expect(page.locator('[data-title-id="PPSA01234"]')).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("ps2-saves")).toHaveCount(0);
});
