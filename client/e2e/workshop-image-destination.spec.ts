// #673: "Convert & upload" sent the image to /data/homebrew with USB set as the drive for packages
// under Volumes, and nothing offered a choice.
import { expect, test } from "@playwright/test";

const HOST = "192.168.1.100";
const USB = "/mnt/usb0";

const volume = (path: string, freeGb: number) => ({
  path,
  fs_type: "exfatfs",
  total_bytes: 900_000_000_000,
  free_bytes: freeGb * 1_000_000_000,
  allocatable_bytes: freeGb * 1_000_000_000,
  writable: true,
  is_placeholder: false,
});

test("an image goes to the drive for packages, shows the path, and the drive can be changed", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1400 });
  await page.addInitScript(
    ({ host, usb }) => {
      window.localStorage.setItem("ps5upload.host", host);
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({ active_id: "p1", profiles: [{ id: "p1", name: "Living room", host }] }),
      );
      window.localStorage.setItem("ps5upload.pkg_storage.v1", JSON.stringify({ [host]: usb }));
      window.localStorage.setItem("ps5upload.convert.make", "image");
    },
    { host: HOST, usb: USB },
  );
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === "/api/version") return route.fulfill(json({ version: "11.1.0", caps: {} }));
      if (path === "/api/ps5/status")
        return route.fulfill(
          json({ command_count: 1, instance_id: 1, prior_instance: "clean", ps5_kernel: "r229358/releases/13.60", ucred_elevated: true, version: "11.1.0" }),
        );
      if (path === "/api/ps5/volumes") return route.fulfill(json({ volumes: [volume("/data", 400), volume(USB, 750)] }));
      if (path === "/api/fpkg/inspect")
        return route.fulfill(
          json({
            source: "folder",
            files: 3,
            bytes: 3 << 20,
            content_id: "UP9000-PPSA01234_00-ASTROBOT00000000",
            title: "Astro Bot",
            planned_size: 3 << 20,
            output_free: 1 << 40,
            checks: [{ name: "eboot.bin", ok: true, detail: "ok" }],
          }),
        );
      return route.fulfill(json({ error: "down" }, 503));
    },
  );
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });
  const source = main.locator("#fpkg-source");
  await source.fill("/games/Astro");
  await source.blur();
  await expect(main.getByText("Astro Bot").first()).toBeVisible();

  const then = main.getByTestId("convert-then");
  await expect(then.locator('option[value="upload"]')).toBeEnabled({ timeout: 20_000 });
  await then.selectOption("upload");

  // The Volumes setting, with its free space, before anything runs.
  const dest = main.getByTestId("image-dest");
  await expect(dest.getByTestId("image-dest-path")).toHaveText(`Goes to ${USB}/homebrew/…`);
  const drive = dest.getByTestId("image-dest-drive");
  await expect(drive).toHaveValue("");
  await expect(drive.locator("option").first()).toContainText(`Default: ${USB}`);
  await expect(drive.locator("option").first()).toContainText("free");

  // Changing the drive and the folder changes the path; /data can be chosen on purpose.
  await drive.selectOption("/data");
  await dest.getByTestId("image-dest-folder").fill("games");
  await expect(dest.getByTestId("image-dest-path")).toHaveText("Goes to /data/games/…");
  await drive.selectOption("");
  await expect(dest.getByTestId("image-dest-path")).toHaveText(`Goes to ${USB}/games/…`);
});
