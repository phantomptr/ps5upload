import { expect, test } from "@playwright/test";

// #536: "Start the queue when you add something". Off: Upload only adds to the queue and
// nothing is sent until Start queue; the switch on the queue panel turns it back on.

test("with queue auto-start off, Upload waits for Start queue", async ({ page }) => {
  const calls: string[] = [];
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem("ps5upload.queue_auto_start", "false");
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const path = url.pathname;
      calls.push(`${route.request().method()} ${path}`);
      // The upload dialog asks whether the destination is there.
      if (path === "/api/ps5/fs/stat") {
        await route.fulfill({
          status: 502,
          contentType: "application/json",
          body: JSON.stringify({ error: "payload rejected FS_STAT: fs_stat_failed_errno_2" }),
        });
        return;
      }
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.2.0", caps: {} };
      if (path === "/api/ps5/status") body = { ok: true, version: "9.2.0" };
      if (path === "/api/files/upload")
        body = { upload_id: "u1", path: "/tmp/u1/notes.txt", size: 5, filename: "notes.txt" };
      if (path === "/api/ps5/volumes")
        body = { volumes: [{ path: "/data", writable: true, is_placeholder: false, free_bytes: 1e12, total_bytes: 2e12 }] };
      if (path.startsWith("/api/transfer/")) body = { job_id: "job-1" };
      if (path.startsWith("/api/jobs/"))
        body = { job_id: "job-1", status: "running", bytes_sent: 2, total_bytes: 5, files_sent: 0, total_files: 1 };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") });
  const upload = page.getByRole("button", { name: "Upload", exact: true });
  await expect(upload).toBeEnabled({ timeout: 30_000 });
  await upload.click();
  await page.getByRole("button", { name: /Start upload/ }).click();

  // Added, not started.
  await expect(page.getByTestId("queue-row").filter({ hasText: "notes.txt" }).first()).toBeVisible();
  const autoStart = page.getByRole("switch", { name: "Start the queue when you add something" }).first();
  await expect(autoStart).toHaveAttribute("aria-checked", "false");
  await page.waitForTimeout(1000);
  expect(calls.filter((c) => c.startsWith("POST /api/transfer/"))).toHaveLength(0);

  // Start queue sends it.
  await page.getByRole("button", { name: "Start queue", exact: true }).click();
  await expect.poll(() => calls.filter((c) => c.startsWith("POST /api/transfer/")).length, { timeout: 30_000 }).toBe(1);
  await expect(page.getByRole("button", { name: "Stop queue", exact: true })).toBeVisible();

  // The switch is the same setting, and it sticks.
  await autoStart.click();
  await expect(autoStart).toHaveAttribute("aria-checked", "true");
  expect(await page.evaluate(() => window.localStorage.getItem("ps5upload.queue_auto_start"))).toBe("true");

  // The queue header's controls fit a phone.
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(autoStart).toBeVisible();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
});
