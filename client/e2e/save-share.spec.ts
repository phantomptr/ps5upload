import { expect, test } from "@playwright/test";

// Sharing a PS5 save with another account (10.0.1): Share saves the save's files as a zip; Use a
// shared save sends a zip into your own save of the same game (the engine backs yours up first).

const HOST = "192.168.1.100";
const IMAGE = "/user/home/10000000/savedata_prospero/PPSA01234/sdimg_SAVEDATA00";

test("Save Data shares a PS5 save and uses a shared one", async ({ page }) => {
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
          return json({ version: "10.0.1", caps: {} });
        case "/api/ps5/status":
          return json({ ok: true });
        case "/api/ps5/saves/list":
          return json({
            saves: [
              { title_id: "PPSA01234", user_id: 0x10000000, path: "/user/home/10000000/savedata_prospero/PPSA01234", size: 4096, mtime: 1761700000, kind: "ps5" },
              { title_id: "CUSA05678", user_id: 0x10000000, path: "/user/home/10000000/savedata/CUSA05678", size: 4096, mtime: 1761600000, kind: "ps4" },
            ],
          });
        case "/api/ps5/list-dir":
          return json({ entries: [{ name: "sdimg_SAVEDATA00", kind: "file", size: 4096, mtime: 1761700000 }], truncated: false });
        case "/api/ps5/saves/open":
          return json({ mounted: false, image: "", mount_point: "", kind: "" });
        case "/api/save-backup/share":
          shared.push(req.postDataJSON());
          return route.fulfill({
            contentType: "application/zip",
            headers: { "content-disposition": 'attachment; filename="PPSA01234 sdimg_SAVEDATA00 shared 20261030-120000.zip"' },
            body: Buffer.from("PK\u0003\u0004 fake zip"),
          });
        case "/api/save-backup/use-shared":
          used.push({ query: url.search, size: req.postDataBuffer()?.length ?? 0 });
          return json({ ok: true });
        default:
          return json({});
      }
    },
  );
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  const share = page.getByTestId("save-share");
  // Only the PS5 save offers it.
  await expect(share).toHaveCount(1, { timeout: 30_000 });
  await expect(page.getByTestId("save-use-shared")).toHaveCount(1);

  const dl = page.waitForEvent("download");
  await share.click();
  expect((await dl).suggestedFilename()).toBe("PPSA01234 sdimg_SAVEDATA00 shared 20261030-120000.zip");
  expect(shared).toEqual([{ addr: HOST, image: IMAGE }]);

  // Use a shared save: pick the file, confirm, and the zip goes to the engine for that image.
  const chooser = page.waitForEvent("filechooser");
  await page.getByTestId("save-use-shared").click();
  await (await chooser).setFiles({ name: "friend.zip", mimeType: "application/zip", buffer: Buffer.from("PK\u0003\u0004 a friend's save") });
  await page.getByRole("button", { name: "Use it" }).click();
  await expect.poll(() => used.length).toBe(1);
  const q = new URLSearchParams(used[0].query);
  expect([q.get("addr"), q.get("image"), q.get("user_id")]).toEqual([HOST, IMAGE, String(0x10000000)]);
  expect(used[0].size).toBeGreaterThan(10);
});
