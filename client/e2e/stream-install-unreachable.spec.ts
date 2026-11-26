import { expect, test, type Page } from "@playwright/test";
import { routeEngine } from "./fixtures/engine";

// #942: Stream & install has the PS5 fetch the package from this computer. When the PS5 could not
// connect (a firewall), the queue row said "The PS5 is installing the package — 0%" for a quarter
// of an hour and never failed. The engine now finds out in seconds and says why; the row shows
// what is waited for while nothing moves, then the reason, with Retry via upload beside it.

interface Fake {
  /** Status polls answered so far. */
  polls: number;
  installs: { source?: unknown }[];
  /** What the engine says once the wait is over. */
  end: Record<string, unknown>;
}

const base = {
  job: "j1",
  ps5_addr: "192.168.1.100",
  content_id: "UP0000-CUSA00001_00-0000000000000000",
  title_id: "CUSA00001",
  route: "stream",
  code: 0,
  metrics: { total_bytes: 0, served_bytes: 0, throughput_mbps: 0, phase_ms: {}, retries: 0, sony_rc: 0 },
  app_ver_before: null,
  app_ver_after: null,
  patch_verdict: null,
  shortened: false,
  started_at: 1,
  updated_at: 1,
};

const failed = (reach: Record<string, unknown>) => ({
  ...base,
  phase: "failed",
  verdict: "failed",
  reason: "stream_unreachable",
  hint: "The PS5 cannot connect to this computer at http://192.168.1.3:19113 (timed out after 4000 ms), so a stream install cannot start.",
  reach: { origin: "http://192.168.1.3:19113", waited_s: 0, source: "host_file", ...reach },
});

async function fakeEngine(page: Page, fake: Fake, waiting: string) {
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status") return { ok: true, version: "12.6.31" };
    if (path === "/api/ps5/volumes") return { volumes: [] };
    if (path === "/api/files/upload") {
      const name = /filename="([^"]+)"/.exec(req.postDataBuffer()?.toString("latin1") ?? "")?.[1] ?? "x";
      return { upload_id: `up-${name}`, path: `/tmp/staged/${name}`, size: 5, filename: name };
    }
    if (path === "/api/pkg/parse-split")
      return {
        head: { content_id: base.content_id, title: "Game A", category: "gd", platform: "ps4" },
        parts: [],
        total_size: 1 << 30,
      };
    if (path === "/api/pkg/install") {
      fake.installs.push(req.postDataJSON() as { source?: unknown });
      return { ok: true, job: "j1" };
    }
    if (path === "/api/pkg/install/status") {
      fake.polls += 1;
      // Two polls of waiting, then the engine's answer.
      return fake.polls <= 2 ? { ...base, phase: "install", verdict: null, reason: null, hint: null, waiting } : fake.end;
    }
    return {};
  });
  await page.goto("/send", { waitUntil: "domcontentloaded" });
}

async function streamInstall(page: Page) {
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles([{ name: "GameA.pkg", mimeType: "application/octet-stream", buffer: Buffer.from("GameA") }]);
  const stream = page.getByTestId("upload-stream-install");
  await expect(stream).toHaveText("Stream & install", { timeout: 20_000 });
  await stream.click();
  await expect(page.getByText(/Stream install from this computer/)).toHaveCount(1, { timeout: 20_000 });
}

test("a PS5 that cannot reach this computer: the row says why at once and offers Retry via upload", async ({ page }) => {
  const fake: Fake = { polls: 0, installs: [], end: failed({ stage: "check", cause: "timed_out" }) };
  await fakeEngine(page, fake, "reach_check");
  await streamInstall(page);

  // While the engine asks the PS5: what is waited for, not "installing — 0%".
  await expect(page.getByText("Checking that the PS5 can reach this computer…").first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText(/The PS5 is installing the package/)).toHaveCount(0);

  // Then the reason, in the row.
  const why = page.getByText(/The PS5 can't connect to this computer \(192\.168\.1\.3:19113\), so it can't fetch the package\./);
  await expect(why.first()).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText(/allow PS5Upload through the firewall/).first()).toBeVisible();
  // The engine's own English (an environment variable, milliseconds) stays in the log.
  await expect(page.getByText(/timed out after 4000 ms/)).toHaveCount(0);
  // The message says it all: the general "if it keeps failing" hint is not added under it.
  await expect(page.getByTestId("stream-fail-hint")).toHaveCount(0);

  // The two ways on: Retry (after fixing the firewall) and Retry via upload, the main one.
  await expect(page.getByRole("button", { name: "Retry", exact: true })).toBeVisible();
  const viaUpload = page.getByRole("button", { name: "Retry via upload", exact: true });
  await expect(viaUpload).toBeVisible();
  // STREAM_SHOTS_DIR=<dir> keeps a picture of the failed row.
  if (process.env.STREAM_SHOTS_DIR) {
    await why.first().scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${process.env.STREAM_SHOTS_DIR}/stream-unreachable.png` });
  }

  // Retry via upload: the package goes to the PS5 as an upload that installs when it lands; the
  // stream row is gone and no second stream install was asked for.
  await viaUpload.click();
  await expect(page.getByText(/Stream install from this computer/)).toHaveCount(0, { timeout: 30_000 });
  await expect(page.getByText(/\/user\/data\/ps5upload\/pkg_library\//).first()).toBeVisible();
  expect(fake.installs).toHaveLength(1);
});

test("a PS5 that was handed the package and never fetched it: the wait is named, then given up on", async ({ page }) => {
  const fake: Fake = {
    polls: 0,
    installs: [],
    end: failed({ stage: "no_fetch", cause: "unknown", waited_s: 60 }),
  };
  await fakeEngine(page, fake, "console_fetch");
  await streamInstall(page);
  await expect(
    page.getByText("Waiting for the PS5 to start fetching the package from this computer…").first(),
  ).toBeVisible({ timeout: 30_000 });
  await expect(
    page
      .getByText(/hadn't fetched any of it from this computer \(192\.168\.1\.3:19113\) after 60 seconds, so the install was stopped\./)
      .first(),
  ).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("button", { name: "Retry via upload", exact: true })).toBeVisible();
});
