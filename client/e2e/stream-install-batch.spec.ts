import { expect, test, type Page } from "@playwright/test";

// #580's report: several packages picked or dropped together could only be uploaded first (the
// review list had no Stream & install since Upload and Install Package became one screen in
// 8.7.0). Packages now Stream & install from the list, in its order; anything else in it uploads
// in the same click.

async function fakeEngine(page: Page) {
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.0.1", caps: {} };
      if (path === "/api/ps5/status") body = { ok: true };
      if (path === "/api/ps5/volumes") body = { volumes: [] };
      if (path === "/api/files/upload") {
        const name = /filename="([^"]+)"/.exec(req.postDataBuffer()?.toString("latin1") ?? "")?.[1] ?? "x";
        body = { upload_id: `up-${name}`, path: `/tmp/staged/${name}`, size: 5, filename: name };
      }
      if (path === "/api/pkg/parse-split") {
        const p = (req.postDataJSON() as { path: string }).path;
        const game = p.includes("GameA") ? "CUSA00001" : "CUSA00002";
        // The name says what it is: an update (gp), a DLC (ac), else a game (gd).
        const category = p.includes("Update") ? "gp" : p.includes("DLC") ? "ac" : "gd";
        body = { head: { content_id: `UP0000-${game}_00-0000000000000000`, title: p, category, platform: "ps4" }, parts: [], total_size: 1 << 30 };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.goto("/send", { waitUntil: "domcontentloaded" });
}

const pkg = (name: string) => ({ name, mimeType: "application/octet-stream", buffer: Buffer.from(name) });

test("several packages Stream & install from the review list; the rest uploads", async ({ page }) => {
  await fakeEngine(page);
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles([
    pkg("GameA.pkg"),
    pkg("GameB.pkg"),
    { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") },
  ]);
  const stream = page.getByTestId("batch-stream-install");
  await expect(stream).toHaveText("Stream & install 2", { timeout: 20_000 });
  await expect(page.getByText(/everything else uploads as usual, in the same click/)).toBeVisible();
  await expect(stream).toBeEnabled();
  await stream.click();
  // Both packages are on the queue as stream installs (nothing copied to the PS5 first), in order.
  const rows = page.getByText(/Stream install from this computer/);
  await expect(rows).toHaveCount(2, { timeout: 20_000 });
  const text = await page.locator("body").innerText();
  expect(text.indexOf("GameA")).toBeGreaterThanOrEqual(0);
  expect(text.indexOf("GameA")).toBeLessThan(text.indexOf("GameB"));
  // The review list is done with.
  await expect(stream).toHaveCount(0);
});

test("one package: Stream & install is the main button, Upload & install the fallback", async ({ page }) => {
  await fakeEngine(page);
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles([pkg("GameA.pkg")]);
  const stream = page.getByTestId("upload-stream-install");
  await expect(stream).toHaveText("Stream & install", { timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Upload & install", exact: true })).toBeVisible();
  await stream.click();
  await expect(page.getByText(/Stream install from this computer/)).toHaveCount(1, { timeout: 20_000 });
});

test("a game streams before its update and DLC, whatever order they were dropped in", async ({ page }) => {
  await fakeEngine(page);
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles([pkg("GameA-Update.pkg"), pkg("GameA-DLC.pkg"), pkg("GameA.pkg")]);
  const stream = page.getByTestId("batch-stream-install");
  await expect(stream).toHaveText("Stream & install 3", { timeout: 20_000 });
  await stream.click();
  const rows = page.getByText(/Stream install from this computer/);
  await expect(rows).toHaveCount(3, { timeout: 20_000 });
  const text = await page.locator("body").innerText();
  const at = (name: string) => text.indexOf(`${name}\n`);
  expect(at("GameA.pkg")).toBeGreaterThanOrEqual(0);
  expect(at("GameA.pkg")).toBeLessThan(at("GameA-Update.pkg"));
  expect(at("GameA-Update.pkg")).toBeLessThan(at("GameA-DLC.pkg"));
});
