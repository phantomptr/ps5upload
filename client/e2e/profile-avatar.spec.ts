import { expect, test } from "@playwright/test";

const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=";

test("browser avatar selection previews local bytes and applies only after confirmation", async ({ page }) => {
  const previews: Record<string, unknown>[] = [];
  const applies: Record<string, unknown>[] = [];
  let rejectPreview = false;
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
    const path = new URL(route.request().url()).pathname;
    let body: unknown = {};
    if (path === "/api/version") body = { version: "7.1.10", caps: {} };
    if (path === "/api/ps5/status") body = { ok: true };
    if (path === "/api/profile/info") body = { ok: true, uid: 123, uid_hex: "0x0000007B", username: "Tester", users: [{ uid: 123, uid_hex: "0x0000007B", username: "Tester" }], slots: [] };
    if (path === "/api/profile/avatar/current") body = { data_url: null };
    if (path === "/api/profile/avatar/preview") {
      previews.push(route.request().postDataJSON());
      if (rejectPreview) return route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: "Invalid image" }) });
      body = { data_url: `data:image/png;base64,${png}` };
    }
    if (path === "/api/profile/avatar") {
      applies.push(route.request().postDataJSON());
      body = { uid: 123, files_copied: 11 };
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto("/profile", { waitUntil: "domcontentloaded" });
  const picker = page.getByRole("button", { name: "Choose image…", exact: true });
  await expect(picker).toBeVisible({ timeout: 30_000 });
  const chooser = page.waitForEvent("filechooser");
  await picker.click();
  await (await chooser).setFiles({ name: "avatar.png", mimeType: "image/png", buffer: Buffer.from(png, "base64") });
  await expect(page.getByText("avatar.png", { exact: true })).toBeVisible();
  await expect.poll(() => previews.length).toBeGreaterThan(0);
  expect(previews.at(-1)?.image_path).toBe(`data:image/png;base64,${png}`);
  await page.getByRole("button", { name: "Fit", exact: true }).click();
  await expect.poll(() => previews.at(-1)?.mode).toBe("fit");
  await page.getByRole("button", { name: "Apply avatar", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  expect(applies).toHaveLength(0);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  expect(applies).toHaveLength(0);
  await page.getByRole("button", { name: "Apply avatar", exact: true }).click();
  await dialog.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect.poll(() => applies.length).toBe(1);
  await expect(page.getByText("Avatar applied (11 files written).", { exact: true })).toBeVisible();
  expect(applies[0]).toMatchObject({ image_path: `data:image/png;base64,${png}`, mode: "fit", uid: 123, username: "Tester" });
  // A failed preview must not allow applying a stale successful selection.
  rejectPreview = true;
  const input = page.locator('input[type="file"]');
  await input.setInputFiles({ name: "broken.png", mimeType: "image/png", buffer: Buffer.from("invalid") });
  await expect(page.getByText(/Invalid image/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply avatar", exact: true })).toBeDisabled();
  const beforeOversized = previews.length;
  await input.setInputFiles({ name: "large.png", mimeType: "image/png", buffer: Buffer.alloc(8 * 1024 * 1024 + 1) });
  await expect(page.getByText("Choose an image of 8 MiB or smaller.", { exact: true })).toBeVisible();
  expect(previews).toHaveLength(beforeOversized);
  expect(applies).toHaveLength(1);

});
