import { expect, test, type Page, type Route } from "@playwright/test";

// #872: a link whose server sends the file only whole (no byte ranges) is still usable, and a
// link can be saved to a PS5 folder without installing it: from Files into the folder that is
// open, and from Send for a package too. The engine here is a fake.

const HOST = "192.168.0.5";
const LINK = "https://files.example.com/dl/Game.pkg?token=abc";

interface Seen {
  downloads: Array<{ url: string; dest_dir: string; file_name: string | null }>;
  installs: unknown[];
}

async function fakeEngine(page: Page, probe: Record<string, unknown>): Promise<Seen> {
  const seen: Seen = { downloads: [], installs: [] };
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: { rar: true }, version: "12.6.12" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          max_transfer_streams: 8,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          started_at_unix: 1791360198,
          ucred_elevated: true,
          version: "12.6.12",
        });
      if (p === "/api/ps5/volumes") return json(route, { volumes: [] });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [{ name: "homebrew", kind: "dir", size: 0, mtime: 1791360199 }],
          truncated: false,
          total_scanned: 1,
          returned: 1,
        });
      if (p === "/api/link/probe") return json(route, probe);
      if (p === "/api/link/download") {
        seen.downloads.push(route.request().postDataJSON());
        return json(route, { job_id: "11111111-2222-3333-4444-555555555555" });
      }
      if (p.startsWith("/api/jobs/"))
        return json(route, { status: "running", bytes_sent: 1024, total_bytes: 4096 });
      if (p === "/api/jobs") return json(route, []);
      if (p.startsWith("/api/pkg/remote/download") || p === "/api/pkg/install/start") {
        seen.installs.push({ path: p, body: route.request().postDataJSON() });
        return json(route, {});
      }
      return json(route, {});
    },
  );
  return seen;
}

const WHOLE_FILE = {
  kind: "file",
  filename: "notes.zip",
  total_size: 4096,
  ranges: false,
  content_type: "application/zip",
};

test("Files: Download from a link saves into the open folder, and says a whole-file server starts over", async ({
  page,
}) => {
  const seen = await fakeEngine(page, WHOLE_FILE);
  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(main.getByText("homebrew", { exact: true })).toBeVisible({ timeout: 30_000 });

  await main.getByRole("button", { name: "More", exact: true }).click();
  await page.getByRole("menuitem", { name: "Download from a link" }).click();
  const ask = page.getByTestId("fs-link-download");
  await expect(ask).toBeVisible();
  // Nothing to check yet.
  await expect(page.getByTestId("fs-link-check")).toBeDisabled();
  await page.getByTestId("fs-link-url").fill(LINK);
  await page.getByTestId("fs-link-check").click();

  await expect(main.getByText("Download this file to the PS5")).toBeVisible();
  await expect(page.getByTestId("linkdl-whole")).toBeVisible();
  const folder = main.getByLabel("Folder on the PS5");
  const dir = await folder.inputValue();
  expect(dir.startsWith("/")).toBe(true);
  await expect(main.getByLabel("File name")).toHaveValue("notes.zip");

  await main.getByRole("button", { name: "Download to the PS5" }).click();
  await expect.poll(() => seen.downloads.length).toBe(1);
  expect(seen.downloads[0]).toMatchObject({ url: LINK, dest_dir: dir, file_name: "notes.zip" });
  await expect(main.getByText(/Downloading to the PS5/)).toBeVisible();
});

test("Send: a package link can be saved to a PS5 folder without installing it", async ({ page }) => {
  const seen = await fakeEngine(page, {
    kind: "pkg",
    filename: "Game.pkg",
    total_size: 1 << 30,
    ranges: true,
    content_type: "application/octet-stream",
  });
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const fold = page.getByText("From a Link or an Archive");
  await expect(fold).toBeVisible({ timeout: 30_000 });
  if (!(await page.getByTestId("link-install-url").isVisible())) await fold.click();
  await page.getByTestId("link-install-url").fill(LINK);
  await page.getByTestId("link-install-save").click();

  await expect(page.getByText("Save this package on the PS5")).toBeVisible();
  // A server with byte ranges resumes: no warning.
  await expect(page.getByTestId("linkdl-whole")).toHaveCount(0);
  await expect(page.getByLabel("File name")).toHaveValue("Game.pkg");
  await page.getByRole("button", { name: "Download to the PS5" }).click();
  await expect.poll(() => seen.downloads.length).toBe(1);
  expect(seen.downloads[0]).toMatchObject({ url: LINK, file_name: "Game.pkg" });
  expect(seen.installs).toEqual([]);
});

test("Send: installing a package from a whole-file server says this computer saves it first", async ({
  page,
}) => {
  const seen = await fakeEngine(page, {
    kind: "pkg",
    filename: "Game.pkg",
    total_size: 1 << 30,
    ranges: false,
    content_type: "application/octet-stream",
  });
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const fold = page.getByText("From a Link or an Archive");
  await expect(fold).toBeVisible({ timeout: 30_000 });
  if (!(await page.getByTestId("link-install-url").isVisible())) await fold.click();
  await page.getByTestId("link-install-url").fill(LINK);
  await page.getByTestId("link-install-go").click();
  await expect(page.getByText(/sends the file only in one piece/)).toBeVisible();
  await expect(page.getByText(/saves the whole package to its disk/)).toBeVisible();
  // Whatever way was chosen, it is the download-first one that starts.
  await page.getByRole("button", { name: "Start install" }).click();
  await expect
    .poll(() => (seen.installs[0] as { path?: string } | undefined)?.path)
    .toBe("/api/pkg/remote/download/start");
});
