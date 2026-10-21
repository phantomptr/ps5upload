import { expect, test } from "@playwright/test";

test("browser payload selection sends bytes only after the Send action", async ({ page }) => {
  let sends = 0;
  let sent: Buffer | null = null;
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
    const path = new URL(route.request().url()).pathname;
    let body: unknown = {};
    if (path === "/api/version") body = { version: "7.1.11", caps: {} };
    if (path === "/api/ps5/status") body = { ok: true };
    if (path === "/api/payload/send") {
      sends++;
      sent = route.request().postDataBuffer();
      body = { bytes_sent: sent?.length };
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto("/payloads?tab=send", { waitUntil: "domcontentloaded" });
  const bytes = Buffer.from([0x7f, 0x45, 0x4c, 0x46, 1, 2, 3]);
  await page.getByLabel("Payload file").setInputFiles({ name: "test.elf", mimeType: "application/octet-stream", buffer: bytes });
  expect(sends).toBe(0);
  await page.getByRole("button", { name: "Send payload", exact: true }).click();
  await expect.poll(() => sends).toBe(1);
  expect(sent).toEqual(bytes);
  await expect(page.getByText("Sent 7 bytes to the loader.", { exact: true })).toBeVisible();
});
