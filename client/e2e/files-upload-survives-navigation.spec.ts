import { expect, test, type Route } from "@playwright/test";

// Regression: an "Add files" upload on the Files screen used to live inside the screen, so
// opening another screen and coming back showed no progress and an armed drop zone, as if
// the upload had been cancelled. The engine here is a fake that answers the same routes.

const HOST = "192.168.0.5";
const PC_DIR = "/pc/games";
const PC_FILE = `${PC_DIR}/big.exfat`;
const TOTAL = 10_000_000_000;

test("an Add files upload keeps its progress across a screen change", async ({
  page,
}) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);

  // The fake engine's one upload job: running until the test lets it finish.
  const job = { started: 0, cancelled: 0, finished: false };
  const json = (route: Route, body: unknown, status = 200) =>
    route.fulfill({
      status,
      contentType: "application/json",
      body: JSON.stringify(body),
    });

  // Only the engine's routes: the dev server also serves the app's own /src/api/ modules.
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      if (p === "/api/version")
        return json(route, { caps: { rar: true }, version: "6.2.3" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          max_transfer_streams: 8,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          started_at_unix: 1791360198,
          ucred_elevated: true,
          version: "6.2.3",
        });
      if (p === "/api/ps5/volumes")
        return json(route, {
          volumes: [
            {
              path: "/data",
              mount_from: "/user/data",
              fs_type: "nullfs",
              total_bytes: 670_000_000_000,
              free_bytes: 400_000_000_000,
              writable: true,
              is_placeholder: false,
              source_image: "",
              safety_reserve_bytes: 1_073_741_824,
              allocatable_bytes: 398_000_000_000,
            },
          ],
        });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [
            { name: "homebrew", kind: "dir", size: 0, mtime: 1791360199 },
          ],
          truncated: false,
          total_scanned: 1,
          returned: 1,
        });
      if (p === "/api/local/storage-roots") return json(route, [PC_DIR]);
      if (p === "/api/local/list-dir")
        return json(route, [
          { name: "big.exfat", path: PC_FILE, is_dir: false, size: TOTAL },
        ]);
      if (p === "/api/local/path-kind") return json(route, { kind: "file" });
      if (p === "/api/transfer/file" && req.method() === "POST") {
        job.started += 1;
        return json(route, { job_id: "job-1" });
      }
      if (p === "/api/jobs/job-1/cancel") {
        job.cancelled += 1;
        return json(route, {});
      }
      if (p === "/api/jobs/job-1")
        return json(
          route,
          job.finished
            ? {
                status: "done",
                bytes_sent: TOTAL,
                total_bytes: TOTAL,
                dest: "/data/big.exfat",
              }
            : {
                status: "running",
                bytes_sent: 2_500_000_000,
                total_bytes: TOTAL,
              },
        );
      if (p === "/api/jobs") return json(route, []);
      // Anything else the screens poll: an empty answer is enough here.
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await main
    .getByRole("button", { name: "Add files" })
    .click({ timeout: 30_000 });

  // The browser build picks from the engine's machine: choose the file there.
  await page.getByText("big.exfat").first().click();
  await page.getByRole("button", { name: "Add 1" }).click();

  const progress = page.getByTestId("fs-upload-progress");
  await expect(progress).toBeVisible({ timeout: 15_000 });
  await expect(progress).toContainText("25%");

  // Leave Files and come back: the same upload, still running, still cancellable.
  const primary = page.getByRole("navigation", { name: "Primary" });
  await primary.getByRole("link", { name: "Volumes", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Volumes" })).toBeVisible();
  // The Files screen is kept alive behind this one, so its progress exists but is hidden.
  await expect(page.getByTestId("fs-upload-progress")).toBeHidden();

  await primary.getByRole("link", { name: "Files", exact: true }).click();
  await expect(progress).toBeVisible({ timeout: 15_000 });
  await expect(progress).toContainText("25%");
  // Further picks join the queue while the current copy stays active.
  await expect(main.getByRole("button", { name: "Add files" })).toBeEnabled();
  await expect(page.getByTestId("fs-upload-queue")).toContainText("big.exfat");

  // From another screen it is visible as a running task.
  await primary.getByRole("link", { name: "Activity & Logs", exact: true }).click();
  // Only the screen on show: the hidden Files screen also names the file.
  await expect(
    page.locator("[data-scroll-root]:visible").getByText("big.exfat").first(),
  ).toBeVisible();
  await primary.getByRole("link", { name: "Files", exact: true }).click();
  await expect(progress).toBeVisible({ timeout: 15_000 });

  // Nothing restarted or cancelled it along the way.
  expect(job.started).toBe(1);
  expect(job.cancelled).toBe(0);

  job.finished = true;
  await expect(progress).toHaveCount(0, { timeout: 15_000 });
  await expect(main.getByRole("button", { name: "Add files" })).toBeEnabled();
  expect(job.started).toBe(1);
});

test("a stopped or interrupted Add files upload can be resumed", async ({
  page,
}) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);

  // The fake engine: every start is a new job that runs until cancelled.
  const jobs: Record<string, "running" | "cancelled"> = {};
  const json = (route: Route, body: unknown, status = 200) =>
    route.fulfill({
      status,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      if (p === "/api/version")
        return json(route, { caps: { rar: true }, version: "6.2.3" });
      if (p === "/api/ps5/status")
        return json(route, {
          instance_id: 1,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          ucred_elevated: true,
          version: "6.2.3",
        });
      if (p === "/api/ps5/volumes") return json(route, { volumes: [] });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [],
          truncated: false,
          total_scanned: 0,
          returned: 0,
        });
      if (p === "/api/local/storage-roots") return json(route, [PC_DIR]);
      if (p === "/api/local/list-dir")
        return json(route, [
          { name: "big.exfat", path: PC_FILE, is_dir: false, size: TOTAL },
        ]);
      if (p === "/api/local/path-kind") return json(route, { kind: "file" });
      if (p === "/api/transfer/file" && req.method() === "POST") {
        const id = `job-${Object.keys(jobs).length + 1}`;
        jobs[id] = "running";
        return json(route, { job_id: id });
      }
      const m = /^\/api\/jobs\/(job-\d+)(\/cancel)?$/.exec(p);
      if (m) {
        if (m[2]) {
          jobs[m[1]] = "cancelled";
          return json(route, {});
        }
        return json(
          route,
          jobs[m[1]] === "running"
            ? {
                status: "running",
                bytes_sent: 2_500_000_000,
                total_bytes: TOTAL,
              }
            : { status: "failed", error: "transfer_cancelled" },
        );
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await main
    .getByRole("button", { name: "Add files" })
    .click({ timeout: 30_000 });
  await page.getByText("big.exfat").first().click();
  await page.getByRole("button", { name: "Add 1" }).click();
  const progress = page.getByTestId("fs-upload-progress");
  await expect(progress).toBeVisible({ timeout: 15_000 });

  // Stopped on purpose: offered again, with no error.
  await progress.getByRole("button", { name: /cancel|stop/i }).click();
  const stopped = page.getByTestId("fs-upload-stopped");
  await expect(stopped).toContainText("You stopped this copy at big.exfat");
  await stopped.getByRole("button", { name: "Resume" }).click();
  await expect(progress).toBeVisible({ timeout: 15_000 });
  await expect(stopped).toHaveCount(0);
  expect(Object.keys(jobs)).toEqual(["job-1", "job-2"]);

  // The app goes away mid-upload and comes back: the run was saved.
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(stopped).toContainText("interrupted when the app closed", {
    timeout: 30_000,
  });
  await stopped.getByRole("button", { name: "Resume" }).click();
  await expect(progress).toBeVisible({ timeout: 15_000 });
  // Its engine job was still running, so it was picked back up, not started a third time.
  expect(Object.keys(jobs)).toEqual(["job-1", "job-2"]);

  // And it can be let go.
  await progress.getByRole("button", { name: /cancel|stop/i }).click();
  await stopped.getByRole("button", { name: "Dismiss" }).click();
  await expect(stopped).toHaveCount(0);
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(main.getByRole("button", { name: "Add files" })).toBeEnabled({
    timeout: 30_000,
  });
  await expect(stopped).toHaveCount(0);
});

// Asked for by a donor: pause a huge copy to put a small file on the console, then carry on.
test("Pause, Resume and Send now on Files uploads", async ({ page }) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const SMALL = `${PC_DIR}/small.txt`;
  // The fake engine: every start is a new job, named after its file, running until the test
  // finishes it or it is cancelled.
  const jobs: { id: string; name: string; state: "running" | "done" | "cancelled" }[] = [];
  const startedNames = () => jobs.map((j) => j.name);
  const finish = (name: string) => {
    for (const j of jobs) if (j.name === name && j.state === "running") j.state = "done";
  };
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: { rar: true }, version: "6.2.3" });
      if (p === "/api/ps5/status")
        return json(route, { instance_id: 1, prior_instance: "clean", ucred_elevated: true, version: "6.2.3" });
      if (p === "/api/ps5/volumes") return json(route, { volumes: [] });
      if (p === "/api/ps5/list-dir")
        return json(route, { path: url.searchParams.get("path") ?? "/data", entries: [], truncated: false, total_scanned: 0, returned: 0 });
      if (p === "/api/local/storage-roots") return json(route, [PC_DIR]);
      if (p === "/api/local/list-dir")
        return json(route, [
          { name: "big.exfat", path: PC_FILE, is_dir: false, size: TOTAL },
          { name: "small.txt", path: SMALL, is_dir: false, size: 5 },
        ]);
      if (p === "/api/local/path-kind") return json(route, { kind: "file" });
      if (p === "/api/transfer/file" && req.method() === "POST") {
        const src = String((req.postDataJSON() as { src: string }).src);
        const id = `job-${jobs.length + 1}`;
        jobs.push({ id, name: src.split("/").pop() ?? "", state: "running" });
        return json(route, { job_id: id });
      }
      const m = /^\/api\/jobs\/(job-\d+)(\/cancel)?$/.exec(p);
      if (m) {
        const job = jobs.find((j) => j.id === m[1]);
        if (m[2]) {
          if (job && job.state === "running") job.state = "cancelled";
          return json(route, {});
        }
        if (job?.state === "done") return json(route, { status: "done", bytes_sent: 5, total_bytes: 5 });
        if (job?.state === "running")
          return json(route, { status: "running", bytes_sent: 2_500_000_000, total_bytes: TOTAL });
        return json(route, { status: "failed", error: "transfer_cancelled" });
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  const addFile = async (name: string) => {
    await main.getByRole("button", { name: "Add files" }).click({ timeout: 30_000 });
    await page.getByRole("dialog").getByText(name).first().click();
    await page.getByRole("button", { name: "Add 1" }).click();
  };
  const progress = page.getByTestId("fs-upload-progress");
  const queue = page.getByTestId("fs-upload-queue");

  // Pause the big copy: it waits as Paused, nothing is lost, the line is free.
  await addFile("big.exfat");
  await expect(progress).toBeVisible({ timeout: 15_000 });
  await progress.getByTestId("fs-upload-pause").click();
  await expect(queue.getByTestId("fs-upload-paused")).toBeVisible();
  await expect(queue).toContainText("big.exfat");
  await expect(progress).toHaveCount(0);
  await expect(page.getByTestId("fs-upload-stopped")).toHaveCount(0);

  // A small file dropped now goes at once and finishes.
  await addFile("small.txt");
  await expect.poll(startedNames).toEqual(["big.exfat", "small.txt"]);
  finish("small.txt");
  await expect(progress).toHaveCount(0, { timeout: 15_000 });
  await expect(queue).not.toContainText("small.txt");

  // Resume: the big copy carries on.
  await queue.getByTestId("fs-upload-resume-item").click();
  await expect(progress).toBeVisible({ timeout: 15_000 });
  await expect.poll(startedNames).toEqual(["big.exfat", "small.txt", "big.exfat"]);
  // The running pick stays listed while it runs, no longer paused.
  await expect(queue.getByTestId("fs-upload-paused")).toHaveCount(0);

  // Send now: a small file queued behind the big copy goes first; the big copy pauses and
  // carries on by itself once it is done.
  await addFile("small.txt");
  await expect(queue).toContainText("small.txt");
  await queue.getByTestId("fs-upload-send-now").click();
  await expect.poll(startedNames).toEqual(["big.exfat", "small.txt", "big.exfat", "small.txt"]);
  await expect(queue).toContainText("Paused for a Send now");
  finish("small.txt");
  await expect.poll(startedNames, { timeout: 15_000 }).toEqual([
    "big.exfat", "small.txt", "big.exfat", "small.txt", "big.exfat",
  ]);
  await expect(progress).toBeVisible();
  await expect(queue.getByTestId("fs-upload-paused")).toHaveCount(0);
  await expect(queue).not.toContainText("small.txt");
  // Never two at once: each earlier job had ended (cancelled or done) before the next began.
  expect(jobs.filter((j) => j.state === "running")).toHaveLength(1);
});
