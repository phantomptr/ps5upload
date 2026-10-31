import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Browser, type Page } from "@playwright/test";

// #538: the whole picture shows, whatever the window, display scaling, codec or the size the
// console really sends. A real H.264/HEVC frame goes through the app's own decode and draw path
// (the engine socket is faked), then the stage is screenshotted and its four corner markers
// looked for where the picture should be.
//
// Fixtures: one frame, grey with a marker in each corner (red, green, blue, white; each 1/8 of
// the height), made with ffmpeg:
//   -f lavfi -i color=c=0x404040:s=WxH -frames:v 1 -vf drawbox... -c:v libx264|libx265
// The 1080p H.264 one is coded 1920x1088 with the bottom 8 rows cropped in its SPS.

const fixture = (name: string) =>
  new Uint8Array(readFileSync(fileURLToPath(new URL(`./fixtures/remote-play/${name}`, import.meta.url))));

/** Split an Annex-B stream into its parameter sets (the "video header") and the picture. */
function split(stream: Uint8Array, hevc: boolean): { header: Uint8Array; au: Uint8Array } {
  for (let i = 0; i + 3 < stream.length; i++) {
    if (stream[i] !== 0 || stream[i + 1] !== 0 || stream[i + 2] !== 1) continue;
    const t = hevc ? (stream[i + 3] >> 1) & 0x3f : stream[i + 3] & 0x1f;
    const isSlice = hevc ? t < 32 : t >= 1 && t <= 5;
    if (!isSlice) continue;
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

type Fit = "fit" | "fill" | "stretch";

interface Case {
  dpr: number;
  /** The stream's own size (the app asks for 1080p). */
  size: "1920x1080" | "1280x720";
  /** Force H.264 (hide HEVC from the page), else whatever the page picks. */
  h264?: boolean;
  fit?: Fit;
  full?: boolean;
  /** Picture tools (lib/rpPicture.ts) set before the stream starts. */
  picture?: Record<string, unknown>;
  /** The canvas may be drawn bigger than the stream (upscaling). */
  anySize?: boolean;
  /** The Windows app's webview (true) or a Mac's (false); unset, the project's own user agent
   *  (Desktop Chrome's is a Windows one). */
  windows?: boolean;
}

async function open(browser: Browser, c: Case): Promise<{ page: Page; close: () => Promise<void> }> {
  const ctx = await browser.newContext({
    viewport: { width: 1919, height: 1040 },
    deviceScaleFactor: c.dpr,
    ...(c.windows === true
      ? { userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0" }
      : c.windows === false
        ? { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36" }
        : {}),
  });
  const page = await ctx.newPage();
  await page.addInitScript(
    ({ fit, h264, picture }) => {
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
              wake_rp_key: "00112233445566778899aabbccddeeff",
            },
          ],
        }),
      );
      window.localStorage.setItem("ps5upload.rp_prefs.v1", JSON.stringify({ fit, ...(picture ? { picture } : {}) }));
      if (h264 && typeof VideoDecoder !== "undefined") {
        const orig = VideoDecoder.isConfigSupported.bind(VideoDecoder);
        VideoDecoder.isConfigSupported = (cfg: VideoDecoderConfig) =>
          /^(hev1|hvc1)/.test(cfg.codec) ? Promise.resolve({ supported: false, config: cfg }) : orig(cfg);
      }
    },
    { fit: c.fit ?? "fit", h264: !!c.h264, picture: c.picture ?? null },
  );
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.2.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m !== "string") return;
      const msg = JSON.parse(m) as { host?: string; codec?: string };
      if (!msg.host) return;
      const hevc = msg.codec === "h265";
      const { header, au } = split(fixture(hevc ? `hevc_${c.size}.hevc` : `h264_${c.size}.h264`), hevc);
      ws.send(JSON.stringify({ stage: "first_frame" }));
      ws.send(record(0, header));
      // The same keyframe a few times: a decoder may hold the first picture back.
      for (let i = 0; i < 6; i++) ws.send(record(1, au));
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  return { page, close: () => ctx.close() };
}

const MARKERS = [
  { name: "top-left", fx: 0, fy: 0, rgb: [255, 0, 0] },
  { name: "top-right", fx: 1, fy: 0, rgb: [0, 255, 0] },
  { name: "bottom-left", fx: 0, fy: 1, rgb: [0, 0, 255] },
  { name: "bottom-right", fx: 1, fy: 1, rgb: [255, 255, 255] },
] as const;

/** Colours at points of the stage (CSS px from its top-left), read off a real screenshot. */
async function colorsAt(page: Page, shot: Buffer, w: number, pts: Array<[number, number]>) {
  return page.evaluate(
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
    { b64: shot.toString("base64"), w, pts },
  );
}

const near = (a: number[], b: readonly number[]) => a.every((v, i) => Math.abs(v - b[i]) <= 48);

async function checkPicture(page: Page, c: Case) {
  const [W, H] = c.size.split("x").map(Number);
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  const canvas = stage.getByTestId("rp-canvas");
  // The canvas takes the frame's own size, not the size asked for.
  if (c.anySize) await expect.poll(() => canvas.evaluate((el: HTMLCanvasElement) => el.width), { timeout: 20_000 }).toBeGreaterThanOrEqual(W);
  else await expect.poll(() => canvas.evaluate((el: HTMLCanvasElement) => `${el.width}x${el.height}`), { timeout: 20_000 }).toBe(c.size);
  await expect(canvas).toHaveAttribute("data-fit", c.fit ?? "fit");
  // Nothing drawn over the picture while it is measured.
  await page.addStyleTag({ content: '[data-testid="rp-stage"] > :not(:first-child) { visibility: hidden !important; }' });

  const sb = (await stage.boundingBox())!;
  const cb = (await canvas.boundingBox())!;
  const fit = c.fit ?? "fit";
  if (fit === "fill") {
    // Fill: the picture's shape, covering the stage (the stage cuts what is past it).
    expect(cb.width).toBeGreaterThanOrEqual(sb.width - 1);
    expect(cb.height).toBeGreaterThanOrEqual(sb.height - 1);
    expect(Math.abs(cb.width / cb.height - W / H)).toBeLessThan(0.01);
  } else {
    // Fit and Stretch: the canvas box never leaves the stage, whatever its pixel size.
    expect(cb.x).toBeGreaterThanOrEqual(sb.x - 1);
    expect(cb.y).toBeGreaterThanOrEqual(sb.y - 1);
    expect(cb.x + cb.width).toBeLessThanOrEqual(sb.x + sb.width + 1);
    expect(cb.y + cb.height).toBeLessThanOrEqual(sb.y + sb.height + 1);
  }

  // Where the picture is: the browser sizes the canvas box for the fit, so the picture fills
  // its box (under object-fit, where container units are missing, the box is the stage and
  // object-fit places it there instead).
  const boxed = await canvas.evaluate((el) => getComputedStyle(el).objectFit === "fill");
  const sx = boxed || fit === "stretch" ? cb.width / W : fit === "fill" ? Math.max(cb.width / W, cb.height / H) : Math.min(cb.width / W, cb.height / H);
  const sy = boxed || fit === "stretch" ? cb.height / H : sx;
  const px = cb.x - sb.x + (cb.width - W * sx) / 2;
  const py = cb.y - sb.y + (cb.height - H * sy) / 2;
  const m = H / 8; // marker side, frame px
  const pts: Array<[number, number]> = MARKERS.map(({ fx, fy }) => [
    px + (fx ? W - m / 2 : m / 2) * sx,
    py + (fy ? H - m / 2 : m / 2) * sy,
  ]);
  // And a point on the stage's left edge, halfway down: a bar with Fit when the stage is wider
  // than the picture, picture with Fill and Stretch.
  pts.push([2, sb.height / 2]);
  for (const [x, y] of pts.slice(0, 4)) {
    expect(x).toBeGreaterThan(0);
    expect(y).toBeGreaterThan(0);
    expect(x).toBeLessThan(sb.width);
    expect(y).toBeLessThan(sb.height);
  }

  const shot = await stage.screenshot({ animations: "disabled" });
  const got = await colorsAt(page, shot, sb.width, pts);
  MARKERS.forEach((mk, i) => expect(near(got[i], mk.rgb), `${mk.name} marker at ${pts[i]} is ${got[i]}`).toBe(true));
  const wider = sb.width / sb.height > W / H + 0.01;
  if (wider) {
    const edge = got[4];
    if (fit === "fit") expect(near(edge, [0, 0, 0]), `bar ${edge}`).toBe(true);
    else expect(near(edge, [0x40, 0x40, 0x40]), `edge ${edge}`).toBe(true);
  }
}

async function startAndCheck(browser: Browser, c: Case) {
  const { page, close } = await open(browser, c);
  try {
    await page.getByTestId("rp-start").click();
    await expect(page.getByTestId("rp-toolbar")).toContainText("Living room");
    if (c.full) {
      await page.getByTestId("rp-stage").filter({ visible: true }).dblclick();
      await expect(page.locator('[data-testid="rp-stage"][data-full]')).toBeVisible();
    }
    await checkPicture(page, c);
  } finally {
    await close();
  }
}

for (const dpr of [1, 1.25, 1.5, 2]) {
  test(`Remote Play shows the whole 1080p H.264 picture at ${dpr * 100}% scaling`, async ({ browser }) => {
    await startAndCheck(browser, { dpr, size: "1920x1080", h264: true });
  });
}

test("Remote Play shows the whole picture with the codec the page picks (HEVC where it decodes)", async ({ browser }) => {
  await startAndCheck(browser, { dpr: 1.5, size: "1920x1080" });
});

test("Remote Play shows a 720p stream whole when 1080p was asked for", async ({ browser }) => {
  await startAndCheck(browser, { dpr: 1.25, size: "1280x720", h264: true });
});

for (const fit of ["fit", "fill", "stretch"] as const) {
  test(`Remote Play full screen, ${fit}`, async ({ browser }) => {
    await startAndCheck(browser, { dpr: 1.5, size: "1920x1080", h264: true, fit, full: true });
  });
}

test("Remote Play picture size is a setting that applies while playing", async ({ browser }) => {
  const { page, close } = await open(browser, { dpr: 1, size: "1920x1080", h264: true });
  try {
    await page.getByTestId("rp-start").click();
    const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
    await expect(canvas).toHaveAttribute("data-fit", "fit");
    const toolbar = page.getByTestId("rp-toolbar");
    await toolbar.hover();
    await toolbar.getByRole("button", { name: "Settings" }).click();
    const select = page.getByTestId("rp-fit");
    await expect(select).toBeEnabled();
    await select.selectOption("fill");
    await expect(canvas).toHaveAttribute("data-fit", "fill");
    // The page's box is the picture's own shape, so Fill is that box, and so is Stretch.
    const stage = page.getByTestId("rp-stage").filter({ visible: true });
    const size = async (el: typeof canvas) => {
      const b = (await el.boundingBox())!;
      return [Math.round(b.width), Math.round(b.height)];
    };
    await expect.poll(() => size(canvas)).toEqual(await size(stage));
    await select.selectOption("stretch");
    await expect(canvas).toHaveAttribute("data-fit", "stretch");
    await expect.poll(() => size(canvas)).toEqual(await size(stage));
    await expect
      .poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_prefs.v1") ?? "{}").fit))
      .toBe("stretch");
  } finally {
    await close();
  }
});

// Picture tools (9.11): the frame goes through the GPU pass (rpPicture.ts) before it reaches the
// page's canvas: cropped to the middle 21:9 band and brightened, here on a real decoded frame.
test("Remote Play picture tools crop to 21:9 and brighten the real picture", async ({ browser }) => {
  const { page, close } = await open(browser, {
    dpr: 1,
    size: "1920x1080",
    h264: true,
    picture: { crop219: true, brightness: 40 },
  });
  try {
    await page.getByTestId("rp-start").click();
    const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
    // 1920 wide, the middle 21:9 band high (1920 * 9 / 21, even).
    await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => `${c.width}x${c.height}`), { timeout: 20_000 }).toBe("1920x822");
    // The fixture's grey (0x40) is brighter now.
    const mid = await canvas.evaluate((c: HTMLCanvasElement) =>
      Array.from(c.getContext("2d")!.getImageData(c.width / 2, c.height / 2, 1, 1).data.slice(0, 3)),
    );
    for (const v of mid) expect(v).toBeGreaterThan(0x40 + 60);
  } finally {
    await close();
  }
});

// The picture tools draw through the GPU pass: the whole picture still shows, at Windows'
// usual 125% and 150% scaling, in every picture size, sharpened or upscaled.
for (const dpr of [1.25, 1.5])
  for (const fit of ["fit", "fill", "stretch"] as const)
    for (const [name, picture] of [
      ["sharpened", { sharpen: 40 }],
      ["upscaled", { upscale: true }],
    ] as const)
      test(`Remote Play shows the whole ${name} picture at ${dpr * 100}%, ${fit}`, async ({ browser }) => {
        await startAndCheck(browser, { dpr, size: "1920x1080", h264: true, fit, picture, anySize: true });
      });

// Windows (#538 again, upscaled): the picture is never drawn bigger than its box in CSS pixels,
// so it cannot show enlarged from the top-left whatever the display scaling, and still shows whole.
for (const dpr of [1.25, 1.5])
  test(`Remote Play upscales on Windows to the box's CSS size at ${dpr * 100}%`, async ({ browser }) => {
    const { page, close } = await open(browser, { dpr, size: "1920x1080", h264: true, picture: { upscale: true }, windows: true });
    try {
      await page.getByTestId("rp-start").click();
      const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
      const read = () =>
        canvas.evaluate((c: HTMLCanvasElement) => {
          const r = c.getBoundingClientRect();
          return { px: [c.width, c.height], css: [Math.round(r.width), Math.round(r.height)] };
        });
      await expect.poll(async () => (await read()).px[0], { timeout: 20_000 }).toBeGreaterThanOrEqual(1920);
      await page.waitForTimeout(500);
      const r = await read();
      // Elsewhere this box is upscaled to its device pixels (1.25x/1.5x its CSS size); on
      // Windows never past the CSS box (here smaller than 1.1x the stream: not upscaled at all).
      expect(r.px[0]).toBeLessThanOrEqual(Math.max(1920, r.css[0] + 2));
      expect(r.px[1]).toBeLessThanOrEqual(Math.max(1080, r.css[1] + 2));
    } finally {
      await close();
    }
    await startAndCheck(browser, { dpr, size: "1920x1080", h264: true, picture: { upscale: true }, windows: true, anySize: true });
  });

// Upscaling (FSR 1): the real 1080p picture on a 2x screen is drawn at the picture's size on
// screen, in device pixels, and keeps its shape (its layout does not move).
test("Remote Play upscales the real picture to its size on screen", async ({ browser }) => {
  // Not Windows: there the picture is upscaled to its CSS size only (above).
  const { page, close } = await open(browser, { dpr: 2, size: "1920x1080", h264: true, picture: { upscale: true }, windows: false });
  try {
    await page.getByTestId("rp-start").click();
    const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
    const read = () =>
      canvas.evaluate((c: HTMLCanvasElement) => {
        const r = c.getBoundingClientRect();
        return {
          px: [c.width, c.height],
          box: [Math.round(r.width * devicePixelRatio), Math.round(r.height * devicePixelRatio)],
          shape: Number(c.style.getPropertyValue("--rp-shape")),
          lit: Array.from(c.getContext("2d")!.getImageData(c.width / 2, c.height / 2, 1, 1).data.slice(0, 3)),
        };
      });
    await expect.poll(async () => (await read()).px[0], { timeout: 20_000 }).toBeGreaterThan(1920 * 1.1);
    const r = await read();
    // Drawn at the box's device pixels (within rounding), the picture's own 16:9 shape kept.
    expect(Math.abs(r.px[0] - r.box[0])).toBeLessThanOrEqual(2);
    expect(Math.abs(r.px[1] - r.box[1])).toBeLessThanOrEqual(2);
    expect(r.shape).toBeCloseTo(16 / 9, 3);
    // The picture is there (the fixture's grey, not black).
    expect(r.lit[1]).toBeGreaterThan(0x20);
  } finally {
    await close();
  }
});
