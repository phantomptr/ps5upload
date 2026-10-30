import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "@playwright/test";

// Watch party (9.10): the computer playing shows a second code, to watch; a phone that scans it
// gets the stream's picture (the engine forwards the session's own messages, rp_stream_api.rs
// to_spectators) and plays only once the computer hands it the controller.

const PAD = "http://192.168.1.9:19113/pad#t=00000000000000000000000000000001";

test("the computer shows a watch code and hands a watcher the controller", async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          {
            id: "p1",
            name: "Living room",
            host: "192.168.1.100",
            wake_regist_key: "00112233",
            wake_rp_key: "00000000000000000000000000000001",
          },
        ],
      }),
    );
  });
  const control: unknown[] = [];
  let hasControl = false;
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.10.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/rp/pad-link/control") {
        const b = req.postDataJSON() as { on: boolean };
        control.push(b);
        hasControl = b.on;
        body = { ok: true };
      } else if (path === "/api/ps5/rp/pad-link") {
        body = {
          open: true,
          url: PAD,
          urls: [PAD],
          pad_ws: "ws://192.168.1.9:19113/api/pad/ws",
          pads: [{ id: 3, name: "iPad", watch: true, control: hasControl }],
          pending: [],
        };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if ((JSON.parse(String(m)) as { host?: string }).host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Remote Play" })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-phones").click();

  // The controller code by default; the watch code is the same link, opened to watch.
  await expect(page.getByTestId("rp-pad-url")).toHaveText(PAD);
  await page.getByTestId("rp-pad-mode-watch").click();
  await expect(page.getByTestId("rp-pad-url")).toHaveText(`${PAD}&w=1`);
  await expect(page.getByTestId("rp-pad-mode-watch")).toHaveAttribute("aria-pressed", "true");

  const list = page.getByTestId("rp-pad-list");
  await expect(list).toContainText("iPad");
  await expect(list).toContainText("watching");
  await page.getByTestId("rp-watch-control").click();
  await expect.poll(() => control).toEqual([{ id: 3, on: true }]);
  await expect(list).toContainText("has the controller");
  await expect(page.getByTestId("rp-watch-control")).toHaveText("Take the controller back");
  await page.getByTestId("rp-watch-control").click();
  await expect.poll(() => control).toEqual([
    { id: 3, on: true },
    { id: 3, on: false },
  ]);
});

/** One second of 320x240 H.264 as the engine forwards it: the header (kind 0), then the
 *  access units (kind 1), the first a keyframe without its parameter sets. */
async function makeStream(page: Page): Promise<number[][]> {
  return page.evaluate(async () => {
    const aus: Uint8Array[] = [];
    const enc = new VideoEncoder({
      output: (c) => {
        const d = new Uint8Array(c.byteLength);
        c.copyTo(d);
        aus.push(d);
      },
      error: (e) => {
        throw e;
      },
    });
    enc.configure({ codec: "avc1.42001f", width: 320, height: 240, framerate: 30, avc: { format: "annexb" } });
    const cv = new OffscreenCanvas(320, 240);
    const g = cv.getContext("2d")!;
    for (let i = 0; i < 30; i++) {
      g.fillStyle = `hsl(${i * 12}, 80%, 50%)`;
      g.fillRect(0, 0, 320, 240);
      const f = new VideoFrame(cv, { timestamp: Math.round((i * 1e6) / 30) });
      enc.encode(f, { keyFrame: i === 0 });
      f.close();
    }
    await enc.flush();
    const first = aus[0];
    let idr = 0;
    for (let i = 0; i + 3 < first.length; i++) {
      if (first[i] === 0 && first[i + 1] === 0 && first[i + 2] === 1 && (first[i + 3] & 0x1f) === 5) {
        idr = i > 0 && first[i - 1] === 0 ? i - 1 : i;
        break;
      }
    }
    const msg = (kind: number, d: Uint8Array) => [kind, ...d];
    return [msg(0, first.subarray(0, idr)), msg(1, first.subarray(idr)), ...aus.slice(1).map((a) => msg(1, a))];
  });
}

test.describe("a phone watching", () => {
  test.use({ viewport: { width: 844, height: 390 }, hasTouch: true, isMobile: true });

  test("shows the picture, and plays once it is handed the controller", async ({ page }) => {
    // The engine's own pad page (npm run build:pad), served as the engine serves it.
    const html = await readFile(new URL("../../engine/crates/ps5upload-engine/src/pad_page.html", import.meta.url), "utf8");
    await page.route("**/pad", (route) => route.fulfill({ contentType: "text/html", body: html }));
    let wsUrl = "";
    const pads: Array<{ buttons: number }> = [];
    let handOver: () => void = () => {};
    let stream: number[][] = [];
    await page.routeWebSocket(/\/api\/pad\/ws/, (ws) => {
      wsUrl = ws.url();
      ws.onMessage((m) => {
        const msg = JSON.parse(String(m)) as { pad?: { buttons: number } };
        if (msg.pad) pads.push(msg.pad);
      });
      ws.send(JSON.stringify({ stage: "joined", session: { host: "192.168.1.100", playing: true } }));
      ws.send(JSON.stringify({ watch: { hevc: false } }));
      for (const m of stream) ws.send(Buffer.from(m));
      handOver = () => ws.send(JSON.stringify({ control: true }));
    });

    await page.goto("/", { waitUntil: "domcontentloaded" });
    stream = await makeStream(page);
    await page.goto("/pad#t=00000000000000000000000000000001&w=1", { waitUntil: "domcontentloaded" });
    await expect(page.getByTestId("pad-stage")).toHaveText("Watching 192.168.1.100", { timeout: 30_000 });
    expect(wsUrl).toContain("w=1");

    // The picture is drawn at the stream's size; a watcher sends nothing and has no controller.
    await expect.poll(() => page.getByTestId("watch-canvas").evaluate((c) => (c as HTMLCanvasElement).width)).toBe(320);
    await expect(page.getByTestId("watch-problem")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "✕" })).toHaveCount(0);
    expect(pads).toEqual([]);

    handOver();
    await expect(page.getByTestId("pad-stage")).toHaveText("Watching, and you have the controller");
    await page.getByRole("button", { name: "✕" }).tap();
    await expect.poll(() => pads.some((p) => p.buttons & 1)).toBe(true);
  });
});
