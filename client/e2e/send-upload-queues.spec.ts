import { expect, test } from "@playwright/test";

// Send to PS5's Upload puts the item in the console's queue and starts it (8.10): its
// progress shows on its queue row, not in a one-shot card of its own.

test("Upload sends through the queue", async ({ page }) => {
  const calls: string[] = [];
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const path = url.pathname;
      calls.push(`${route.request().method()} ${path}`);
      let body: unknown = {};
      if (path === "/api/version") body = { version: "8.10.0", caps: {} };
      if (path === "/api/ps5/status") body = { ok: true, version: "8.10.0" };
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
  // The destination check asks first (Settings → Upload can turn this off).
  await page.getByRole("button", { name: /Start upload/ }).click();
  // One transfer, started by the queue, and the item is a queue row.
  await expect.poll(() => calls.filter((c) => c.startsWith("POST /api/transfer/")).length, { timeout: 30_000 }).toBe(1);
  await expect(page.getByTestId("queue-row").filter({ hasText: "notes.txt" }).first()).toBeVisible();
});

// Asked for by a donor: pause a huge upload to send a small file first, then carry on.
test("Pause, Resume and Send now on queue rows", async ({ page }) => {
  const HOST = "192.168.1.100";
  const row = (name: string) =>
    ({
      id: `q-${name}`, sourceKind: "file", sourcePath: `/pc/${name}`, displayName: name,
      resolvedDest: `/data/${name}`, addr: HOST, strategy: "resume", reconcileMode: "fast",
      excludes: [], mountAfterUpload: false, mountReadOnly: false, registerAfterUpload: false,
      txIdHex: `tx${name.replace(/\W/g, "")}`, status: "pending", bytesSent: 0, totalBytes: 0,
      bytesPerSec: 0, filesFinalized: 0, filesFinalizingTotal: 0, mountedAt: null,
      registeredAs: null, mountWarnings: [], error: null, errorReason: null, errorDetail: null,
      addedAt: 1, startedAt: null, completedAt: null,
    });
  // Seeded once: a reload must find what the app saved, not the seed again.
  await page.addInitScript((doc) => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    if (!window.sessionStorage.getItem("seeded")) {
      window.sessionStorage.setItem("seeded", "1");
      window.localStorage.setItem("ps5upload.uploadQueue", JSON.stringify(doc));
    }
  }, { items: [row("big.bin"), row("small.txt"), row("other.txt")], continueOnFailure: false });

  // Every start is a new job named after its file; it runs until finished or cancelled.
  const jobs: { id: string; name: string; tx: string; state: "running" | "done" | "cancelled" }[] = [];
  const started = () => jobs.map((j) => j.name);
  const finish = (name: string) => {
    for (const j of jobs) if (j.name === name && j.state === "running") j.state = "done";
  };
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.4.1", caps: {} };
      if (path === "/api/ps5/status") body = { ok: true, version: "10.4.1" };
      if (path === "/api/ps5/volumes")
        body = { volumes: [{ path: "/data", writable: true, is_placeholder: false, free_bytes: 1e12, total_bytes: 2e12 }] };
      if (path === "/api/transfer/file" && req.method() === "POST") {
        const { src, tx_id } = req.postDataJSON() as { src: string; tx_id: string };
        const id = `job-${jobs.length + 1}`;
        jobs.push({ id, name: src.split("/").pop() ?? "", tx: tx_id, state: "running" });
        body = { job_id: id };
      }
      const m = /^\/api\/jobs\/(job-\d+)(\/cancel)?$/.exec(path);
      if (m) {
        const job = jobs.find((j) => j.id === m[1]);
        if (m[2]) {
          if (job?.state === "running") job.state = "cancelled";
        } else if (job?.state === "running") {
          body = { job_id: job.id, status: "running", bytes_sent: 2, total_bytes: 10, files_sent: 0, total_files: 1 };
        } else if (job?.state === "done") {
          body = { job_id: job.id, status: "done", bytes_sent: 10, total_bytes: 10, files_sent: 1, total_files: 1 };
        } else {
          body = { job_id: m[1], status: "failed", error: "transfer_cancelled" };
        }
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const queueRow = (name: string) => page.getByTestId("queue-row").filter({ hasText: name }).first();
  await page.getByRole("button", { name: "Start queue", exact: true }).click({ timeout: 30_000 });
  await expect.poll(started, { timeout: 30_000 }).toEqual(["big.bin"]);

  // Pause the big one: it waits as Paused and the next row goes.
  await queueRow("big.bin").getByTestId("queue-pause").click();
  await expect(queueRow("big.bin").getByTestId("queue-paused")).toBeVisible();
  await expect.poll(started, { timeout: 15_000 }).toEqual(["big.bin", "small.txt"]);
  finish("small.txt");
  await expect.poll(started, { timeout: 15_000 }).toEqual(["big.bin", "small.txt", "other.txt"]);
  finish("other.txt");
  // Only the paused row is left, and nothing picks it.
  await page.waitForTimeout(2500);
  expect(started()).toHaveLength(3);

  // A paused row is still paused after the app restarts.
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(queueRow("big.bin").getByTestId("queue-paused")).toBeVisible({ timeout: 30_000 });

  // Resume: it carries on under the same job id, so the console skips what it has.
  await queueRow("big.bin").getByTestId("queue-resume-paused").click();
  await expect.poll(started, { timeout: 15_000 }).toEqual(["big.bin", "small.txt", "other.txt", "big.bin"]);
  expect(jobs[3].tx).toBe(jobs[0].tx);
  await expect(queueRow("big.bin").getByTestId("queue-paused")).toHaveCount(0);
});

test("Send now runs a row first and resumes the paused upload by itself", async ({ page }) => {
  const HOST = "192.168.1.100";
  const row = (name: string) =>
    ({
      id: `q-${name}`, sourceKind: "file", sourcePath: `/pc/${name}`, displayName: name,
      resolvedDest: `/data/${name}`, addr: HOST, strategy: "resume", reconcileMode: "fast",
      excludes: [], mountAfterUpload: false, mountReadOnly: false, registerAfterUpload: false,
      txIdHex: `tx${name.replace(/\W/g, "")}`, status: "pending", bytesSent: 0, totalBytes: 0,
      bytesPerSec: 0, filesFinalized: 0, filesFinalizingTotal: 0, mountedAt: null,
      registeredAs: null, mountWarnings: [], error: null, errorReason: null, errorDetail: null,
      addedAt: 1, startedAt: null, completedAt: null,
    });
  await page.addInitScript((doc) => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem("ps5upload.uploadQueue", JSON.stringify(doc));
  }, { items: [row("big.bin"), row("other.txt"), row("small.txt")], continueOnFailure: false });
  const jobs: { id: string; name: string; tx: string; state: "running" | "done" | "cancelled" }[] = [];
  const started = () => jobs.map((j) => j.name);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.4.1", caps: {} };
      if (path === "/api/ps5/status") body = { ok: true, version: "10.4.1" };
      if (path === "/api/transfer/file" && req.method() === "POST") {
        const { src, tx_id } = req.postDataJSON() as { src: string; tx_id: string };
        const id = `job-${jobs.length + 1}`;
        jobs.push({ id, name: src.split("/").pop() ?? "", tx: tx_id, state: "running" });
        body = { job_id: id };
      }
      const m = /^\/api\/jobs\/(job-\d+)(\/cancel)?$/.exec(path);
      if (m) {
        const job = jobs.find((j) => j.id === m[1]);
        if (m[2]) {
          if (job?.state === "running") job.state = "cancelled";
        } else if (job?.state === "running") {
          body = { job_id: job.id, status: "running", bytes_sent: 2, total_bytes: 10, files_sent: 0, total_files: 1 };
        } else if (job?.state === "done") {
          body = { job_id: job.id, status: "done", bytes_sent: 10, total_bytes: 10, files_sent: 1, total_files: 1 };
        } else {
          body = { job_id: m[1], status: "failed", error: "transfer_cancelled" };
        }
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const queueRow = (name: string) => page.getByTestId("queue-row").filter({ hasText: name }).first();
  await page.getByRole("button", { name: "Start queue", exact: true }).click({ timeout: 30_000 });
  await expect.poll(started, { timeout: 30_000 }).toEqual(["big.bin"]);

  await queueRow("small.txt").getByTestId("queue-send-now").click();
  await expect(queueRow("big.bin")).toContainText("Paused for a Send now");
  await expect.poll(started, { timeout: 15_000 }).toEqual(["big.bin", "small.txt"]);
  expect(jobs[0].state).toBe("cancelled");
  jobs[1].state = "done";
  // big carries on by itself (ahead of other.txt), under its own job id.
  await expect.poll(started, { timeout: 15_000 }).toEqual(["big.bin", "small.txt", "big.bin"]);
  expect(jobs[2].tx).toBe(jobs[0].tx);
  await expect(queueRow("big.bin").getByTestId("queue-paused")).toHaveCount(0);
});
