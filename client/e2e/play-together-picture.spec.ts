import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// #929: a friend in Play Together (the app, joined from an invite) saw the host's 1080p picture
// enlarged from its top-left corner and cut off on the right, on a Windows PC. Chromium's
// hardware decoder on Windows reports a smaller visible picture than the stream's (#538/#575);
// the host's player puts that right from the stream's SPS, the friend's player drew what the
// decoder said. A real 1080p frame (a marker in each corner, as remote-play-picture.spec.ts)
// goes through the friend's decode and draw path, with the decoder acting out that bug, and the
// whole picture has to be in the window, 16:9, every corner where it should be.

const INVITE = "00112233445566778899aabbccddeeff";
const LINK = `http://192.168.1.9:19113/join#i=${INVITE}&n=Ann`;

const fixture = (name: string) =>
  new Uint8Array(readFileSync(fileURLToPath(new URL(`./fixtures/remote-play/${name}`, import.meta.url))));

/** The H.264 stream's parameter sets (the video header) and its picture. */
function split(stream: Uint8Array): { header: Uint8Array; au: Uint8Array } {
  for (let i = 0; i + 3 < stream.length; i++) {
    if (stream[i] !== 0 || stream[i + 1] !== 0 || stream[i + 2] !== 1) continue;
    const t = stream[i + 3] & 0x1f;
    if (t < 1 || t > 5) continue;
    const start = i > 0 && stream[i - 1] === 0 ? i - 1 : i;
    return { header: stream.slice(0, start), au: stream.slice(start) };
  }
  throw new Error("no picture in the fixture");
}

const record = (kind: number, payload: Uint8Array) => {
  const b = Buffer.alloc(payload.length + 1);
  b[0] = kind;
  b.set(payload, 1);
  return b;
};

const MARKERS = [
  { name: "top-left", fx: 0, fy: 0, rgb: [255, 0, 0] },
  { name: "top-right", fx: 1, fy: 0, rgb: [0, 255, 0] },
  { name: "bottom-left", fx: 0, fy: 1, rgb: [0, 0, 255] },
  { name: "bottom-right", fx: 1, fy: 1, rgb: [255, 255, 255] },
] as const;
const near = (a: number[], b: readonly number[]) => a.every((v, i) => Math.abs(v - b[i]) <= 48);

/** How the decoder acts: "edge" says each frame shows only its top-left 1280x720; "crops" (in
 *  hardware only) hands out just that part as a frame of its own, nothing to put right. */
type Decoder = "plain" | "edge" | "crops";

/** The friend's app at the reporter's window size, joined to Ann's game. */
async function joined(page: Page, mode: Decoder) {
  await seedConsole(page);
  await page.addInitScript(
    ({ mode }) => {
      window.localStorage.setItem("ps5upload.join.name", "Ben");
      window.localStorage.setItem("ps5upload.join.guided", "1");
      const w = window as unknown as { VideoDecoder: typeof VideoDecoder; __accel: string[] };
      w.__accel = [];
      if (mode === "plain" || typeof VideoDecoder === "undefined") return;
      const Real = VideoDecoder;
      class WindowsDecoder extends Real {
        private hw = true;
        constructor(init: VideoDecoderInit) {
          super({
            ...init,
            output: (f: VideoFrame) => {
              if (mode === "edge") {
                const small = new VideoFrame(f, {
                  visibleRect: { x: 0, y: 0, width: 1280, height: 720 },
                  displayWidth: 1280,
                  displayHeight: 720,
                });
                f.close();
                init.output(small);
              } else if (this.hw) {
                const c = new OffscreenCanvas(1280, 720);
                c.getContext("2d")!.drawImage(f, 0, 0, 1280, 720, 0, 0, 1280, 720);
                const small = new VideoFrame(c, { timestamp: f.timestamp });
                f.close();
                init.output(small);
              } else init.output(f);
            },
          });
        }
        configure(cfg: VideoDecoderConfig) {
          w.__accel.push(cfg.hardwareAcceleration ?? "no-preference");
          this.hw = cfg.hardwareAcceleration !== "prefer-software";
          super.configure(cfg);
        }
      }
      w.VideoDecoder = WindowsDecoder;
    },
    { mode },
  );
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.27", caps: {} };
    if (path === "/api/ps5/rp/session") return { host: null };
    return {};
  });
  const { header, au } = split(fixture("h264_1920x1080.h264"));
  await page.routeWebSocket(/\/api\/join\/relay/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m !== "string") return;
      const v = JSON.parse(m) as { hello?: unknown; ping?: number; keyframe?: boolean };
      if (v.ping !== undefined) ws.send(JSON.stringify({ pong: v.ping }));
      if (v.keyframe) for (let i = 0; i < 4; i++) ws.send(record(1, au));
      if (!v.hello) return;
      ws.send(JSON.stringify({ stage: "joined", player: 2, token: "APP", hevc: false }));
      ws.send(JSON.stringify({ status: { player: 2, state: "playing", rtt_ms: 12, host: "Ann" } }));
      // The picture a moment later, once the view is up; the same keyframe a few times (a decoder
      // may hold the first one back).
      setTimeout(() => {
        ws.send(record(0, header));
        for (let i = 0; i < 6; i++) ws.send(record(1, au));
      }, 500);
    });
  });
  await page.goto(`/play?join=${encodeURIComponent(LINK)}`, { waitUntil: "domcontentloaded" });
  await page.getByTestId("join-invite-go").click({ timeout: 30_000 });
  await expect(page.getByTestId("join-player")).toHaveText("Playing at Ann's · You are Player 2", { timeout: 30_000 });
}

/** The whole picture, inside the window and the stage, 16:9, each corner marker in its place. */
async function wholePicture(page: Page) {
  const canvas = page.getByTestId("watch-canvas");
  await expect
    .poll(() => canvas.evaluate((c: HTMLCanvasElement) => `${c.width}x${c.height}`), { timeout: 20_000 })
    .toBe("1920x1080");
  // Nothing drawn over the picture while it is measured.
  await page.addStyleTag({ content: '[data-testid="join-stage-view"] > :not(:first-child) { visibility: hidden !important; }' });
  const vp = page.viewportSize()!;
  const stage = (await page.getByTestId("join-stage-view").boundingBox())!;
  const cb = (await canvas.boundingBox())!;
  for (const box of [stage, { x: 0, y: 0, width: vp.width, height: vp.height }]) {
    expect(cb.x).toBeGreaterThanOrEqual(box.x - 1);
    expect(cb.y).toBeGreaterThanOrEqual(box.y - 1);
    expect(cb.x + cb.width).toBeLessThanOrEqual(box.x + box.width + 1);
    expect(cb.y + cb.height).toBeLessThanOrEqual(box.y + box.height + 1);
  }
  // object-contain inside the canvas box: the picture is the box's 16:9 middle.
  const s = Math.min(cb.width / 1920, cb.height / 1080);
  const px = cb.x + (cb.width - 1920 * s) / 2;
  const py = cb.y + (cb.height - 1080 * s) / 2;
  const m = 1080 / 8;
  const pts = MARKERS.map(({ fx, fy }) => [px + (fx ? 1920 - m / 2 : m / 2) * s, py + (fy ? 1080 - m / 2 : m / 2) * s]);
  const shot = await page.screenshot({ animations: "disabled" });
  const got = await page.evaluate(
    async ({ b64, w, pts }) => {
      const img = new Image();
      img.src = `data:image/png;base64,${b64}`;
      await img.decode();
      const k = img.naturalWidth / w;
      const cv = document.createElement("canvas");
      cv.width = img.naturalWidth;
      cv.height = img.naturalHeight;
      const g = cv.getContext("2d")!;
      g.drawImage(img, 0, 0);
      return pts.map(([x, y]) => Array.from(g.getImageData(Math.floor(x * k), Math.floor(y * k), 1, 1).data.slice(0, 3)));
    },
    { b64: shot.toString("base64"), w: vp.width, pts },
  );
  MARKERS.forEach((mk, i) => expect(near(got[i], mk.rgb), `${mk.name} marker at ${pts[i]} is ${got[i]}`).toBe(true));
}

test.describe("a friend's picture in the app (#929)", () => {
  test.use({ viewport: { width: 1892, height: 1003 } });

  for (const edge of [false, true])
    test(`the whole 1080p picture fits the window${edge ? ", when the decoder reports a smaller one" : ""}`, async ({ page }) => {
      await joined(page, edge ? "edge" : "plain");
      await wholePicture(page);
    });

  test("the decoder starts over in software when its picture cannot be put right", async ({ page }) => {
    await joined(page, "crops");
    await wholePicture(page);
    const accel = await page.evaluate(() => (window as unknown as { __accel: string[] }).__accel);
    expect(accel[0]).not.toBe("prefer-software");
    expect(accel.at(-1)).toBe("prefer-software");
  });

  test("the whole picture fits in full screen, when the decoder reports a smaller one", async ({ page }) => {
    await joined(page, "edge");
    await page.getByTestId("join-stage-view").hover();
    await page.getByRole("button", { name: "Full screen" }).first().click();
    await expect(page.getByTestId("join-stage-view")).toHaveClass(/fixed/);
    await wholePicture(page);
  });
});
