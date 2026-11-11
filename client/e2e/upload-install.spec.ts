import { expect, test } from "@playwright/test";

// Upload & Install (route /send): the drop zone is the one way in and says what every kind of
// item becomes; the fake-package note is the first thing on the page.

test("Upload & Install opens with the FPKG note and one drop zone", async ({ page }) => {
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      const body = path === "/api/version" ? { version: "8.10.0", caps: {} } : {};
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Upload & Install" })).toBeVisible({ timeout: 30_000 });

  // The FPKG note comes before the drop zone and states the firmware limit without opening.
  const note = page.getByTestId("fpkg-note");
  await expect(note).toContainText("not supported yet on firmware later than 11.60");
  await expect(note).toContainText("PS4 fake packages are fine");
  const drop = page.getByTestId("send-drop");
  const [noteBox, dropBox] = [await note.boundingBox(), await drop.boundingBox()];
  expect(noteBox!.y).toBeLessThan(dropBox!.y);

  // The old three-choice intro is gone; the drop zone covers every kind.
  await expect(page.getByTestId("send-intro")).toHaveCount(0);
  await expect(drop).toContainText("packages, games, disk images, archives or any file");
  const kinds = page.getByTestId("send-drop-kinds");
  for (const text of ["Packages (.pkg, .fpkg)", "game dumps", ".ffpfsc", "Archives (.zip, .7z, .rar, .tar, .tar.zst", "Any other file"]) {
    await expect(kinds).toContainText(text);
  }
  await expect(drop.getByRole("button", { name: "Choose folder" })).toBeVisible();
  await expect(drop.getByRole("button", { name: "Add games from a folder…" })).toBeVisible();
  await expect(drop).toContainText("Copying from another PS5?");

  // No horizontal overflow at phone width.
  await page.setViewportSize({ width: 380, height: 800 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await page.screenshot({ path: process.env.SEND_SHOT ?? "/dev/null", fullPage: true }).catch(() => {});
});
