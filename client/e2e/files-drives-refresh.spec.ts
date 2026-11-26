import { expect, test, type Page, type Route } from "@playwright/test";

// The free space on Files' drive chips is the console's free space of now (#944): Refresh reads
// it again, a finished upload reads it again, and Files and Volumes show the same reading. Files
// used to keep the figure read when it opened, whatever happened after. The engine here is a fake.

const HOST = "192.168.0.5";
const GIB = 1024 ** 3;
const PC_DIR = "/home/me/games";
const PC_FILE = `${PC_DIR}/big.exfat`;
const TOTAL = 10_000_000_000;

/** What the fake console reports, changed by the tests as if space was used or freed. */
interface Fake {
  freeBytes: number;
  volumeReads: number;
  jobFinished: boolean;
}

async function fakeEngine(page: Page, fake: Fake) {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  // Only the engine's routes: the dev server also serves the app's own /src/api/ modules.
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: { rar: true }, version: "6.5.2" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          max_transfer_streams: 8,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          started_at_unix: 1791360198,
          ucred_elevated: true,
          version: "6.5.2",
        });
      if (p === "/api/ps5/volumes") {
        fake.volumeReads += 1;
        return json(route, {
          volumes: [
            {
              path: "/data",
              mount_from: "/user/data",
              fs_type: "nullfs",
              total_bytes: 670 * GIB,
              free_bytes: fake.freeBytes,
              writable: true,
              is_placeholder: false,
              source_image: "",
              safety_reserve_bytes: GIB,
              allocatable_bytes: Math.max(0, fake.freeBytes - GIB),
            },
          ],
        });
      }
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [{ name: "homebrew", kind: "dir", size: 0, mtime: 1791360199 }],
          truncated: false,
          total_scanned: 1,
          returned: 1,
        });
      if (p === "/api/local/storage-roots") return json(route, [PC_DIR]);
      if (p === "/api/local/list-dir")
        return json(route, [{ name: "big.exfat", path: PC_FILE, is_dir: false, size: TOTAL }]);
      if (p === "/api/local/path-kind") return json(route, { kind: "file" });
      if (p === "/api/transfer/file" && req.method() === "POST") return json(route, { job_id: "job-1" });
      if (p === "/api/jobs/job-1")
        return json(
          route,
          fake.jobFinished
            ? { status: "done", bytes_sent: TOTAL, total_bytes: TOTAL, dest: "/data/big.exfat" }
            : { status: "running", bytes_sent: 2_500_000_000, total_bytes: TOTAL },
        );
      if (p === "/api/jobs") return json(route, []);
      // Anything else the screens poll: an empty answer is enough here.
      return json(route, {});
    },
  );
}

/** The /data chip in Files' drive bar. */
const dataChip = (page: Page) => page.getByRole("main").locator("button.chip", { hasText: "/data" });

test("Refresh in Files reads the drives' free space again, and Volumes shows the same", async ({ page }) => {
  // 98.1 GiB, then 163 GiB: the figures of the report.
  const fake: Fake = { freeBytes: Math.round(98.1 * GIB), volumeReads: 0, jobFinished: false };
  await fakeEngine(page, fake);

  await page.goto("/files?path=/data", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(dataChip(page)).toContainText("98.1 GiB", { timeout: 30_000 });
  await expect(dataChip(page)).toHaveAttribute("title", "/data · 98.1 GiB free");

  // Space was freed on the console. Nothing on screen has asked again yet.
  fake.freeBytes = 175_467_069_440;
  await main.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(dataChip(page)).toContainText("163 GiB");
  await expect(dataChip(page)).toHaveAttribute("title", "/data · 163 GiB free");

  // Volumes shows that reading, and its own Refresh a newer one.
  const primary = page.getByRole("navigation", { name: "Primary" });
  await primary.getByRole("link", { name: "Volumes", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Volumes" })).toBeVisible();
  await expect(main.getByText("163 GiB free of 670 GiB")).toBeVisible();
  fake.freeBytes = 150 * GIB;
  await main.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(main.getByText("150 GiB free of 670 GiB")).toBeVisible();

  // Back on Files: what Volumes just read, not what Files last showed.
  await primary.getByRole("link", { name: "Files", exact: true }).click();
  await expect(dataChip(page)).toContainText("150 GiB");
  await expect(dataChip(page)).not.toContainText("163");

  // And a screen that comes back into view asks again by itself.
  await primary.getByRole("link", { name: "Volumes", exact: true }).click();
  await expect(main.getByText("150 GiB free of 670 GiB")).toBeVisible();
  fake.freeBytes = 140 * GIB;
  await primary.getByRole("link", { name: "Files", exact: true }).click();
  await expect(dataChip(page)).toContainText("140 GiB");
});

test("an upload that finishes updates the free space on the drive chip", async ({ page }) => {
  const fake: Fake = { freeBytes: 163 * GIB, volumeReads: 0, jobFinished: false };
  await fakeEngine(page, fake);

  await page.goto("/files?path=/data", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(dataChip(page)).toContainText("163 GiB", { timeout: 30_000 });

  await main.getByRole("button", { name: "Add files" }).click();
  // The browser build picks from the engine's machine: choose the file there.
  await page.getByText("big.exfat").first().click();
  await page.getByRole("button", { name: "Add 1" }).click();
  const progress = page.getByTestId("fs-upload-progress");
  await expect(progress).toBeVisible({ timeout: 15_000 });

  // The copy lands and the drive has less room. No Refresh is pressed.
  fake.freeBytes = 153 * GIB;
  fake.jobFinished = true;
  await expect(dataChip(page)).toContainText("153 GiB", { timeout: 20_000 });
});

test("opening folders does not read the drives each time", async ({ page }) => {
  const fake: Fake = { freeBytes: 163 * GIB, volumeReads: 0, jobFinished: false };
  await fakeEngine(page, fake);

  await page.goto("/files?path=/data", { waitUntil: "domcontentloaded" });
  await expect(dataChip(page)).toContainText("163 GiB", { timeout: 30_000 });
  const before = fake.volumeReads;
  const homebrew = page.locator('[data-fs-name="homebrew"]');
  for (let i = 0; i < 3; i++) {
    await homebrew.locator("button[data-fs-open]").click();
    await expect(page.getByRole("button", { name: "homebrew", exact: true }).first()).toBeVisible();
    await page.locator("body").click({ position: { x: 1, y: 1 } });
    await page.keyboard.press("Backspace");
    await expect(homebrew).toHaveAttribute("data-fs-revealed", "", { timeout: 10_000 });
  }
  // A reading of a moment ago is kept while browsing.
  expect(fake.volumeReads).toBe(before);
});
