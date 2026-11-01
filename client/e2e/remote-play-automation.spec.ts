import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

// #615: a local script asks the engine for the picture (GET /api/ps5/rp/frame); the engine asks
// the page that shows the session ({"snapshot": …} on its socket) and the page answers with what
// its canvas shows, as binary [9][u32 BE meta length][meta JSON][image]. A real H.264 frame goes
// through the app's own decode and draw path (the engine socket is faked), then a snapshot is
// asked for and its pixels checked. Nothing on screen may change.

const fixture = new Uint8Array(
  readFileSync(fileURLToPath(new URL("./fixtures/remote-play/h264_1280x720.h264", import.meta.url))),
);

/** Parameter sets and the picture of an Annex-B H.264 stream. */
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

type Reply = { meta: Record<string, unknown>; image: Buffer };

async function open(page: Page): Promise<{ ws: () => WebSocketRoute; replies: Reply[]; texts: string[] }> {
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
            wake_rp_key: "0f".repeat(16), // made up (built here so it reads as no secret)
          },
        ],
      }),
    );
    // H.264 only: the fixture is H.264.
    if (typeof VideoDecoder !== "undefined") {
      const orig = VideoDecoder.isConfigSupported.bind(VideoDecoder);
      VideoDecoder.isConfigSupported = (cfg: VideoDecoderConfig) =>
        /^(hev1|hvc1)/.test(cfg.codec) ? Promise.resolve({ supported: false, config: cfg }) : orig(cfg);
    }
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.6.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null, session: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const replies: Reply[] = [];
  const texts: string[] = [];
  let socket: WebSocketRoute | null = null;
  const { header, au } = split(fixture);
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    socket = ws;
    ws.onMessage((m) => {
      if (typeof m !== "string") {
        const b = Buffer.from(m);
        if (b[0] !== 9) return;
        const n = b.readUInt32BE(1);
        replies.push({ meta: JSON.parse(b.subarray(5, 5 + n).toString()) as Record<string, unknown>, image: b.subarray(5 + n) });
        return;
      }
      texts.push(m);
      const msg = JSON.parse(m) as { host?: string; keyframe?: boolean };
      if (msg.keyframe) {
        for (let i = 0; i < 6; i++) ws.send(record(1, au));
        return;
      }
      if (!msg.host) return;
      ws.send(JSON.stringify({ stage: "first_frame" }));
      ws.send(record(0, header));
      for (let i = 0; i < 6; i++) ws.send(record(1, au));
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
  await expect.poll(() => canvas.evaluate((el: HTMLCanvasElement) => `${el.width}x${el.height}`), { timeout: 20_000 }).toBe("1280x720");
  return {
    ws: () => {
      if (!socket) throw new Error("no socket");
      return socket;
    },
    replies,
    texts,
  };
}

/** RGB at fractions of a PNG/JPEG, decoded in the page. */
async function colorsAt(page: Page, img: Buffer, mime: string, pts: Array<[number, number]>) {
  return page.evaluate(
    async ({ b64, mime, pts }) => {
      const el = new Image();
      el.src = `data:${mime};base64,${b64}`;
      await el.decode();
      const cv = document.createElement("canvas");
      cv.width = el.naturalWidth;
      cv.height = el.naturalHeight;
      const g = cv.getContext("2d")!;
      g.drawImage(el, 0, 0);
      return {
        w: el.naturalWidth,
        h: el.naturalHeight,
        rgb: pts.map(([fx, fy]) =>
          Array.from(g.getImageData(Math.floor(fx * (cv.width - 1)), Math.floor(fy * (cv.height - 1)), 1, 1).data.slice(0, 3)),
        ),
      };
    },
    { b64: img.toString("base64"), mime, pts },
  );
}

const near = (a: number[], b: number[]) => a.every((v, i) => Math.abs(v - b[i]) <= 48);
// Corner markers of the fixture (red, green, blue, white), sampled a little inside each corner.
const CORNERS: Array<[number, number]> = [
  [0.02, 0.02],
  [0.98, 0.02],
  [0.02, 0.98],
  [0.98, 0.98],
];
const COLORS = [
  [255, 0, 0],
  [0, 255, 0],
  [0, 0, 255],
  [255, 255, 255],
];

test("the page answers a snapshot request with the picture it shows, scaled as asked", async ({ page }) => {
  const { ws, replies, texts } = await open(page);
  const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
  const before = await canvas.boundingBox();

  // PNG scaled to 640 wide.
  ws().send(JSON.stringify({ snapshot: { id: 7, format: "png", max_width: 640 } }));
  await expect.poll(() => replies.length).toBe(1);
  const png = replies[0];
  expect(png.meta).toMatchObject({ id: 7, format: "png", width: 640, height: 360 });
  expect(png.meta.frame_seq as number).toBeGreaterThan(0);
  expect(png.meta.captured_at_ms as number).toBeGreaterThanOrEqual(png.meta.frame_at_ms as number);
  expect(png.image.subarray(0, 4).toString("hex")).toBe("89504e47");
  const p = await colorsAt(page, png.image, "image/png", CORNERS);
  expect([p.w, p.h]).toEqual([640, 360]);
  p.rgb.forEach((c, i) => expect(near(c, COLORS[i]), `corner ${i}: ${c}`).toBe(true));

  // JPEG at the picture's own size.
  ws().send(JSON.stringify({ snapshot: { id: 8, format: "jpeg" } }));
  await expect.poll(() => replies.length).toBe(2);
  const jpg = replies[1];
  expect(jpg.meta).toMatchObject({ id: 8, format: "jpeg", width: 1280, height: 720 });
  expect(jpg.image.subarray(0, 2).toString("hex")).toBe("ffd8");
  const j = await colorsAt(page, jpg.image, "image/jpeg", CORNERS);
  j.rgb.forEach((c, i) => expect(near(c, COLORS[i]), `corner ${i}: ${c}`).toBe(true));

  // Nothing newer than a picture far ahead arrives: said so after the wait asked for.
  ws().send(JSON.stringify({ snapshot: { id: 9, format: "png", after_seq: 1_000_000, wait_ms: 100 } }));
  await expect.poll(() => replies.length).toBe(3);
  expect(replies[2].meta).toEqual({ id: 9, error: "no_newer_frame" });

  // A newer picture after the last one: the page waits for it and answers with it.
  const seq = png.meta.frame_seq as number;
  ws().send(JSON.stringify({ snapshot: { id: 10, format: "png", after_seq: seq, wait_ms: 1500, max_width: 64 } }));
  ws().send(record(1, split(fixture).au));
  await expect.poll(() => replies.length).toBe(4);
  expect(replies[3].meta.id).toBe(10);
  expect(replies[3].meta.frame_seq as number).toBeGreaterThan(seq);

  // The shown picture, its size and the page's messages to the engine are as they were.
  await expect.poll(() => canvas.evaluate((el: HTMLCanvasElement) => `${el.width}x${el.height}`)).toBe("1280x720");
  expect(await canvas.boundingBox()).toEqual(before);
  expect(texts.some((t) => t.includes("snapshot"))).toBe(false);
  await expect(page.getByTestId("rp-stage").filter({ visible: true })).toBeVisible();
});
