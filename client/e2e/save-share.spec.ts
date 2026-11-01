import { expect, test } from "@playwright/test";

// Sharing a PS5 save (10.0.1; one file with every part of the save since 10.6.0): Copy to… ›
// Save as a file saves the whole save as a zip; Copy to… › Load a save file sends a zip into this
// save (the engine backs it up first). No picker: every part goes, matched by name.

const HOST = "192.168.1.100";
const FOLDER = "/user/home/10000000/savedata_prospero/PPSA01234";

test("Save Data shares a PS5 save and loads a save file", async ({ page }) => {
  const used: Array<{ query: string; size: number }> = [];
  const shared: unknown[] = [];
  await page.addInitScript((h) => window.localStorage.setItem("ps5upload.host", h), HOST);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const json = (b: unknown) => route.fulfill({ contentType: "application/json", body: JSON.stringify(b) });
      switch (url.pathname) {
        case "/api/version":
          return json({ version: "10.6.0", caps: {} });
        case "/api/ps5/status":
          return json({ ok: true });
        case "/api/ps5/saves/list":
          return json({
            saves: [
              { title_id: "PPSA01234", user_id: 0x10000000, path: FOLDER, size: 4096, mtime: 1761700000, kind: "ps5" },
              { title_id: "CUSA05678", user_id: 0x10000000, path: "/user/home/10000000/savedata/CUSA05678", size: 4096, mtime: 1761600000, kind: "ps4" },
            ],
          });
        case "/api/ps5/saves/open":
          return json({ mounted: false, image: "", mount_point: "", kind: "" });
        case "/api/save-backup/share":
          shared.push(req.postDataJSON());
          return route.fulfill({
            contentType: "application/zip",
            headers: { "content-disposition": 'attachment; filename="PPSA01234 save shared 20261030-120000.zip"' },
            body: Buffer.from("PK\u0003\u0004 fake zip"),
          });
        case "/api/save-backup/use-shared":
          used.push({ query: url.search, size: req.postDataBuffer()?.length ?? 0 });
          return json({ ok: true, parts: ["sdimg_SAVEDATA00"] });
        default:
          return json({});
      }
    },
  );
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  // Only the PS5 save offers Copy to….
  const copyTo = page.getByRole("button", { name: "Move or share this save" });
  await expect(copyTo).toHaveCount(1, { timeout: 30_000 });

  const dl = page.waitForEvent("download");
  await copyTo.click();
  await page.getByTestId("save-share").click();
  expect((await dl).suggestedFilename()).toBe("PPSA01234 save shared 20261030-120000.zip");
  expect(shared).toEqual([{ addr: HOST, folder: FOLDER }]);

  // Load a save file: the file chooser opens straight from the menu, no picker; confirm, and the
  // zip goes to the engine for the whole save folder.
  const chooser = page.waitForEvent("filechooser");
  await copyTo.click();
  await page.getByTestId("save-use-shared").click();
  await (await chooser).setFiles({ name: "friend.zip", mimeType: "application/zip", buffer: Buffer.from("PK\u0003\u0004 a friend's save") });
  await page.getByRole("button", { name: "Use it" }).click();
  await expect.poll(() => used.length).toBe(1);
  const q = new URLSearchParams(used[0].query);
  expect([q.get("addr"), q.get("folder"), q.get("user_id")]).toEqual([HOST, FOLDER, String(0x10000000)]);
  expect(used[0].size).toBeGreaterThan(10);
});
