import { expect, test } from "@playwright/test";

// Reported on 7.1.9: compressed images are absent despite being visible in Files.
// Exercise the browser/engine path and prevent an unsupported native writable mount.
test("Game files finds compressed images and directs editing through decompression", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("ps5upload.host", "192.0.2.8"));
  await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
    const url = new URL(route.request().url());
    let body: unknown = {};
    if (url.pathname === "/api/version") body = { version: "7.1.10", caps: {} };
    if (url.pathname === "/api/ps5/status") body = { version: "7.1.10", command_count: 1, instance_id: 1, ucred_elevated: true };
    if (url.pathname === "/api/ps5/volumes") body = { volumes: [{ path: "/data", fs_type: "nullfs", writable: true, is_placeholder: false }] };
    if (url.pathname === "/api/ps5/list-dir") body = { entries: [
      { name: "game.FFPFSC", kind: "file", size: 123456 },
    ], truncated: false };
    if (url.pathname === "/api/ps5/smp/status") body = { running: false, installed: false, mounted_images: [], errors: [] };
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto("/games?tab=files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(main.getByText("game.FFPFSC", { exact: true })).toBeVisible({ timeout: 30_000 });
  await expect(main.getByRole("button", { name: "Mount", exact: true })).toBeDisabled();
  await expect(main.getByRole("button", { name: "Edit files", exact: true })).toHaveCount(0);
  await expect(main.getByRole("link", { name: "Open in Files" })).toHaveAttribute("href", "/files?path=%2Fdata");
});
