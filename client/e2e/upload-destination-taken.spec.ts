import { expect, test, type Page } from "@playwright/test";

// A transfer that fails because a copy with the same name is already in that folder on the PS5
// shows Replace and Upload somewhere else on its queue row (a plain Retry fails the same way),
// and the Upload screen asks up front when it can already see that copy.

const HOST = "192.168.1.100";
const VOLUMES = {
  volumes: [
    { path: "/data", writable: true, is_placeholder: false, free_bytes: 1e12, total_bytes: 2e12 },
    { path: "/mnt/ext1", writable: true, is_placeholder: false, free_bytes: 1e12, total_bytes: 2e12 },
  ],
};

type Answer = { status?: number; body: unknown } | undefined;

async function routeApi(page: Page, answer: (path: string, url: URL, body: unknown) => Answer) {
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      let body: unknown = null;
      try {
        body = req.postDataJSON();
      } catch {
        /* no JSON body */
      }
      const a = answer(url.pathname, url, body);
      await route.fulfill({
        status: a?.status ?? 200,
        contentType: "application/json",
        body: JSON.stringify(a?.body ?? {}),
      });
    },
  );
}

const missing = { status: 502, body: { error: "payload rejected FS_LIST_DIR: fs_list_not_found" } };

test("a failed row whose destination is taken offers Replace, which deletes it and sends again", async ({ page }) => {
  const failedRow = {
    id: "q-notes", sourceKind: "file", sourcePath: "/pc/notes.txt", displayName: "notes.txt",
    resolvedDest: "/data/homebrew/notes.txt", addr: HOST, strategy: "overwrite", reconcileMode: "fast",
    excludes: [], mountAfterUpload: false, mountReadOnly: false, registerAfterUpload: false,
    txIdHex: "aa".repeat(16), status: "failed", bytesSent: 5, totalBytes: 5, bytesPerSec: 0,
    filesFinalized: 0, filesFinalizingTotal: 0, mountedAt: null, registeredAs: null, mountWarnings: [],
    error: "the console refused to commit the transfer: the destination already exists",
    errorReason: "ava1_commit_exists", errorDetail: null, addedAt: 1, startedAt: 2, completedAt: 3,
  };
  await page.addInitScript((doc) => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    if (!window.sessionStorage.getItem("seeded")) {
      window.sessionStorage.setItem("seeded", "1");
      window.localStorage.setItem("ps5upload.uploadQueue", JSON.stringify(doc));
    }
  }, { items: [failedRow], continueOnFailure: false });

  const deleted: string[] = [];
  const transfers: { dest: string; tx: string }[] = [];
  await routeApi(page, (path, url, body) => {
    if (path === "/api/version") return { body: { version: "12.6.2", caps: {} } };
    if (path === "/api/ps5/status") return { body: { ok: true, version: "12.6.2" } };
    if (path === "/api/ps5/volumes") return { body: VOLUMES };
    if (path === "/api/ps5/list-dir") {
      // A folder is in the way where the file goes; no part is left.
      if (url.searchParams.get("path") === "/data/homebrew")
        return { body: { entries: deleted.length ? [] : [{ name: "notes.txt", kind: "dir", size: 0 }] } };
      return missing;
    }
    if (path === "/api/ps5/fs/delete") {
      const p = (body as { path: string }).path;
      deleted.push(p);
      return p === "/data/homebrew/notes.txt" ? { body: {} } : { status: 502, body: { error: "fs_delete_not_found" } };
    }
    if (path === "/api/transfer/file") {
      const b = body as { dest: string; tx_id: string };
      transfers.push({ dest: b.dest, tx: b.tx_id });
      return { body: { job_id: "job-2" } };
    }
    if (path.startsWith("/api/jobs/"))
      return { body: { job_id: "job-2", status: "done", bytes_sent: 5, total_bytes: 5, files_sent: 1, total_files: 1 } };
    return undefined;
  });

  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const row = page.getByTestId("queue-row").filter({ hasText: "notes.txt" }).first();
  await expect(row).toContainText("Replace it, or upload somewhere else", { timeout: 30_000 });
  await expect(row).not.toContainText("Override");
  await expect(row.getByTestId("queue-conflict-elsewhere")).toBeVisible();

  // The buttons wrap on a phone, with nothing past the edge.
  await page.setViewportSize({ width: 360, height: 800 });
  await expect(row.getByTestId("queue-conflict-replace")).toBeVisible();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
  await page.setViewportSize({ width: 1280, height: 800 });

  await row.getByTestId("queue-conflict-replace").click();
  const confirm = page.getByRole("alertdialog");
  await expect(confirm).toContainText("Replace /data/homebrew/notes.txt on the PS5? The copy there is deleted.");
  expect(deleted).toEqual([]);
  await confirm.getByRole("button", { name: "Replace", exact: true }).click();

  // Deleted, then sent again to the same place as a new job.
  await expect.poll(() => transfers.length, { timeout: 30_000 }).toBe(1);
  expect(deleted[0]).toBe("/data/homebrew/notes.txt");
  expect(transfers[0].dest).toBe("/data/homebrew/notes.txt");
  expect(transfers[0].tx).not.toBe("aa".repeat(16));
  await expect(row.getByTestId("queue-conflict-replace")).toHaveCount(0, { timeout: 30_000 });
});

test("Upload asks up front when the file is already there, and Another folder sends it elsewhere", async ({ page }) => {
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  const statted: string[] = [];
  const transfers: string[] = [];
  await routeApi(page, (path, url, body) => {
    if (path === "/api/version") return { body: { version: "12.6.2", caps: {} } };
    if (path === "/api/ps5/status") return { body: { ok: true, version: "12.6.2" } };
    if (path === "/api/files/upload")
      return { body: { upload_id: "u1", path: "/tmp/u1/notes.txt", size: 5, filename: "notes.txt" } };
    if (path === "/api/ps5/volumes") return { body: VOLUMES };
    if (path === "/api/ps5/fs/stat") {
      const p = (body as { path: string }).path;
      statted.push(p);
      // Already in the first folder; the second one is clear.
      return statted.length === 1
        ? { body: { kind: "file", size: 5, mtime: 1, mode: 0o644, dev: 1 } }
        : { status: 502, body: { error: "payload rejected FS_STAT: fs_stat_failed_errno_2" } };
    }
    if (path === "/api/ps5/list-dir") {
      if (url.searchParams.get("path") === "/mnt/ext1/homebrew") return { body: { entries: [] } };
      return { body: { entries: [{ name: "games", kind: "dir", size: 0 }] } };
    }
    if (path.startsWith("/api/transfer/")) {
      transfers.push((body as { dest: string }).dest);
      return { body: { job_id: "job-1" } };
    }
    if (path.startsWith("/api/jobs/"))
      return { body: { job_id: "job-1", status: "running", bytes_sent: 2, total_bytes: 5, files_sent: 0, total_files: 1 } };
    return undefined;
  });

  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") });
  const upload = page.getByRole("button", { name: "Upload", exact: true });
  await expect(upload).toBeEnabled({ timeout: 30_000 });
  await upload.click();

  const ask = page.getByRole("alertdialog");
  await expect(ask.getByRole("heading", { name: "Already on the PS5" })).toBeVisible();
  await expect(ask).toContainText("/notes.txt");
  await expect(ask.getByRole("button", { name: /^Replace/ })).toBeVisible();
  await expect(ask.getByRole("button", { name: /^Cancel/ })).toBeVisible();
  // A single file has nothing to resume.
  await expect(ask.getByRole("button", { name: /^Resume/ })).toHaveCount(0);
  await ask.getByTestId("upload-dialog-another-folder").click();

  const picker = page.getByTestId("upload-elsewhere-dialog");
  await picker.getByRole("button", { name: /\/mnt\/ext1\/homebrew/ }).click();
  await expect(picker).toContainText("It goes to /mnt/ext1/homebrew/notes.txt");
  await page.getByTestId("upload-elsewhere-confirm").click();

  // The new place is checked again: clear, so the usual start question.
  await expect.poll(() => statted.length, { timeout: 15_000 }).toBe(2);
  expect(statted[1]).toBe("/mnt/ext1/homebrew/notes.txt");
  await page.getByRole("button", { name: /Start upload/ }).click();
  await expect.poll(() => transfers, { timeout: 30_000 }).toEqual(["/mnt/ext1/homebrew/notes.txt"]);
});
