import { expect, test, type Page } from "@playwright/test";

// Hush (#561): it stubs the video encoder Remote Play needs. Start asks to pause it, pausing
// writes enabled=0 and waits for its log to say the encoder is back, the stream starts, and
// when the stream ends Hush goes back on (enabled=1) because Play paused it.

const HOST = "192.168.1.100";
const CONFIG = "# hush\nenabled=1\n# mode=check reports what would be patched\nmode=apply\nverbose=0\ninterval_s=20\nrescan_s=300\n";
const LOG = "2026-10-31 10:00:00 hush started; firmware=13.60 (0x13600007) enabled=1 mode=apply\n";

interface Console {
  files: Map<string, string>;
  /** Hush's process is up. */
  running: boolean;
  writes: Array<{ path: string; text: string }>;
  deletes: string[];
}

async function hushConsole(page: Page): Promise<Console> {
  const c: Console = {
    files: new Map([
      ["/data/hush/config.ini", CONFIG],
      ["/data/hush/pid.lock", "142\n"],
      ["/data/hush/log.txt", LOG],
    ]),
    running: true,
    writes: [],
    deletes: [],
  };
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          {
            id: "p1",
            name: "Living room",
            host,
            wake_regist_key: "00112233",
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
  }, HOST);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.0.3", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/process/list")
        body = {
          truncated: false,
          processes: c.running
            ? [{ pid: 142, name: "payload.elf", comm: "payload.elf", title_id: "", app_id: 0, memory_mib: 1, threads: 1, kind: "payload" }]
            : [],
        };
      if (path === "/api/ps5/fs/read-preview") {
        const text = c.files.get((req.postDataJSON() as { path: string }).path);
        if (text === undefined) {
          await route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "not found" }) });
          return;
        }
        body = { size: text.length, base64: Buffer.from(text).toString("base64") };
      }
      if (path === "/api/ps5/fs/write-bytes") {
        const { path: file, bytes_b64 } = req.postDataJSON() as { path: string; bytes_b64: string };
        const text = Buffer.from(bytes_b64, "base64").toString("utf8");
        c.writes.push({ path: file, text });
        c.files.set(file, text);
        // Hush reads enabled=0 on its next check: it gives the encoder back and exits.
        if (file === "/data/hush/config.ini" && text.includes("\nenabled=0\n")) {
          c.files.set(
            "/data/hush/log.txt",
            `${LOG}2026-10-31 10:00:20 sceVencStartEncode restored in 4 slot(s)\n2026-10-31 10:00:20 disabled in config, exiting\n`,
          );
          c.files.delete("/data/hush/pid.lock");
          c.running = false;
        }
        body = { ok: true, size: text.length };
      }
      if (path === "/api/ps5/fs/delete") {
        const file = (req.postDataJSON() as { path: string }).path;
        c.deletes.push(file);
        c.files.delete(file);
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  return c;
}

test("Start asks to pause Hush, pauses it for the stream and turns it back on after", async ({ page }) => {
  const c = await hushConsole(page);
  const starts: unknown[] = [];
  let stops = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) {
        starts.push(msg);
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
      if (msg.stop) {
        stops++;
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();

  // Hush runs with enabled=1: the stream waits for the user's choice.
  await expect(page.getByText("Hush is on. It blocks Remote Play's picture.")).toBeVisible();
  expect(starts).toHaveLength(0);
  await page.getByTestId("rp-blocker-pause").click();

  // Paused: only the enabled line changed, and our marker is in our own folder.
  await expect.poll(() => c.writes.map((w) => w.path)).toContain("/data/ps5upload/rp-paused/hush");
  expect(c.writes[0]).toEqual({ path: "/data/hush/config.ini", text: CONFIG.replace("\nenabled=1\n", "\nenabled=0\n") });

  // Its log says the encoder is back: the stream starts.
  await expect.poll(() => starts.length, { timeout: 15_000 }).toBe(1);
  const toolbar = page.getByTestId("rp-toolbar");
  await expect(toolbar).toBeVisible();

  // The stream ends: Hush goes back on, and our marker goes.
  await toolbar.hover();
  await page.getByTestId("rp-stop").click();
  await expect.poll(() => stops).toBe(1);
  await expect.poll(() => c.files.get("/data/hush/config.ini"), { timeout: 15_000 }).toBe(CONFIG);
  await expect.poll(() => c.deletes).toContain("/data/ps5upload/rp-paused/hush");
});

test("Start anyway leaves Hush alone", async ({ page }) => {
  const c = await hushConsole(page);
  let started = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if ("host" in (JSON.parse(String(m)) as Record<string, unknown>)) started++;
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-blocker-anyway").click();
  await expect.poll(() => started).toBe(1);
  // While it plays, the page says why there may be no picture.
  await expect(page.getByText("This stream may have no picture.", { exact: false })).toBeVisible();
  expect(c.writes).toEqual([]);
});
