import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

// #670, #649: the picture on this device off. The stream goes on for friends and the broadcast;
// here a calm placeholder says so and one click brings the picture back, without a restart. The
// engine socket is faked, with a real H.264 picture, so the decoder and canvas really run.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const stream = new Uint8Array(
  readFileSync(fileURLToPath(new URL("./fixtures/remote-play/h264_1280x720.h264", import.meta.url))),
);

function split(s: Uint8Array): { header: Uint8Array; au: Uint8Array } {
  for (let i = 0; i + 3 < s.length; i++) {
    if (s[i] !== 0 || s[i + 1] !== 0 || s[i + 2] !== 1) continue;
    const t = s[i + 3] & 0x1f;
    if (t < 1 || t > 5) continue;
    const start = i > 0 && s[i - 1] === 0 ? i - 1 : i;
    return { header: s.slice(0, start), au: s.slice(start) };
  }
  throw new Error("no picture in the fixture");
}

const record = (kind: number, payload: Uint8Array) => {
  const b = Buffer.alloc(payload.length + 1);
  b[0] = kind;
  b.set(payload, 1);
  return b;
};

type Msg = Record<string, unknown>;

async function fakeEngine(page: Page, session: Msg = { host: null }) {
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
            wake_rp_key: "0f".repeat(16),
          },
        ],
      }),
    );
    // What the page has running: its decoders and sound.
    const w = window as unknown as { __decoders: VideoDecoder[]; __audio: AudioContext[] };
    w.__decoders = [];
    w.__audio = [];
    const Dec = window.VideoDecoder;
    window.VideoDecoder = class extends Dec {
      constructor(init: VideoDecoderInit) {
        super(init);
        w.__decoders.push(this);
      }
    } as typeof VideoDecoder;
    const Actx = window.AudioContext;
    window.AudioContext = class extends Actx {
      constructor(o?: AudioContextOptions) {
        super(o);
        w.__audio.push(this);
      }
    } as typeof AudioContext;
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "11.1.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = session;
      if (path === "/api/stream/status") return route.fulfill({ status: 404, body: "{}" });
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const sent: Msg[] = [];
  const socket: { ws: WebSocketRoute | null } = { ws: null };
  const { header, au } = split(stream);
  const picture = (ws: WebSocketRoute) => {
    for (let i = 0; i < 6; i++) ws.send(record(1, au));
  };
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    socket.ws = ws;
    let off = false;
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Msg;
      sent.push(msg);
      if ("host" in msg) {
        off = msg.picture === false;
        ws.send(JSON.stringify({ stage: "first_frame" }));
        if (off) ws.send(JSON.stringify({ picture: false }));
        else {
          ws.send(record(0, header));
          picture(ws);
        }
      }
      if (msg.picture === false) {
        off = true;
        ws.send(JSON.stringify({ picture: false }));
      }
      if (msg.picture === true) {
        off = false;
        ws.send(JSON.stringify({ picture: true }));
        ws.send(record(0, header));
      }
      if (msg.keyframe && !off) picture(ws);
      if (msg.stop) {
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });
  return { sent, socket };
}

const running = (page: Page) =>
  page.evaluate(() => {
    const w = window as unknown as { __decoders: VideoDecoder[]; __audio: AudioContext[] };
    return {
      decoders: w.__decoders.filter((d) => d.state !== "closed").length,
      audio: w.__audio.filter((a) => a.state !== "closed").length,
    };
  });

const canvasSize = (page: Page) =>
  page.evaluate(() => {
    const c = document.querySelector<HTMLCanvasElement>("canvas[data-testid=rp-canvas]");
    return c ? c.width : -1;
  });

async function start(page: Page): Promise<void> {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toContainText("Living room");
}

test("the picture goes off, this device stops working on it, and one click brings it back", async ({ page }) => {
  const engine = await fakeEngine(page);
  await start(page);
  await expect.poll(() => canvasSize(page), { timeout: 20_000 }).toBe(1280);
  expect(await running(page)).toEqual({ decoders: 1, audio: 1 });
  await expect(page.getByTestId("rp-picture-off")).toHaveCount(0);

  await page.getByTestId("rp-stage").hover();
  await page.getByTestId("rp-picture-button").click();
  await expect(page.getByTestId("rp-picture-off")).toBeVisible();
  await expect(page.getByTestId("rp-picture-off")).toContainText("The picture is off on this device");
  await expect.poll(() => engine.sent.some((m) => m.picture === false)).toBe(true);
  await expect.poll(() => running(page)).toEqual({ decoders: 0, audio: 0 });
  await expect.poll(() => canvasSize(page)).toBe(0);
  // Nothing about a picture is on offer, and no message about a stuck one comes.
  await expect(page.getByTestId("rp-toggle-stats")).toHaveCount(0);
  await expect(page.getByTestId("rp-volume")).toHaveCount(0);
  await expect(page.getByTestId("rp-pip")).toHaveCount(0);
  await expect(page.getByTestId("rp-stop")).toBeVisible();

  // The keyboard reaches nothing, and the session stays: watchdogs have nothing to say.
  const before = engine.sent.length;
  await page.getByTestId("rp-stage").click({ position: { x: 5, y: 200 } });
  await page.keyboard.down("KeyW");
  await page.waitForTimeout(3000);
  await page.keyboard.up("KeyW");
  const after = engine.sent.slice(before);
  expect(after.filter((m) => "pad" in m)).toEqual([]);
  expect(after.filter((m) => m.keyframe)).toEqual([]);
  expect(after.filter((m) => m.stop)).toEqual([]);
  await expect(page.getByTestId("rp-picture-off")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);

  // One click: the same session, the picture back from a keyframe.
  const starts = engine.sent.filter((m) => "host" in m).length;
  await page.getByTestId("rp-picture-on").click();
  await expect(page.getByTestId("rp-picture-off")).toHaveCount(0);
  await expect.poll(() => engine.sent.some((m) => m.picture === true)).toBe(true);
  await expect.poll(() => engine.sent.some((m) => m.keyframe === true)).toBe(true);
  await expect.poll(() => canvasSize(page), { timeout: 20_000 }).toBe(1280);
  expect(await running(page)).toEqual({ decoders: 1, audio: 1 });
  expect(engine.sent.filter((m) => "host" in m).length).toBe(starts);
  expect(engine.sent.filter((m) => m.stop)).toEqual([]);
});

test("the setting starts a stream with no picture here, and the panels offer the switch", async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.rp_prefs.v1", JSON.stringify({ pictureHere: false }));
  });
  const engine = await fakeEngine(page);
  await start(page);
  await expect(page.getByTestId("rp-picture-off")).toBeVisible();
  expect(engine.sent.find((m) => "host" in m)?.picture).toBe(false);
  expect(await running(page)).toEqual({ decoders: 0, audio: 0 });
  // No floating window for a picture that isn't there; the bar leads back.
  await page.evaluate(() => {
    window.history.pushState({}, "", "/games");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect(page.getByTestId("rp-banner")).toBeVisible();
  await expect(page.getByTestId("rp-mini")).toHaveCount(0);
  await page.evaluate(() => {
    window.history.pushState({}, "", "/play");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  // The note about playing on the TV, where Play Together is hosted.
  await page.getByTestId("rp-together").click();
  await expect(page.getByTestId("pt-panel")).toBeVisible();
  await expect(page.getByTestId("rp-picture-toggle")).toBeVisible();
  await expect(page.getByTestId("pt-tv-note")).toContainText("pick a different user");
  await page.getByTestId("rp-together").click();
  await page.getByTestId("rp-golive").click();
  await expect(page.getByTestId("golive-panel").getByTestId("rp-picture-toggle")).toBeVisible();
  // Turned on there, for this session only: the setting keeps what the next one starts with.
  await page.getByTestId("golive-panel").getByTestId("rp-picture-toggle").locator("input, button").first().click();
  await expect(page.getByTestId("rp-picture-off")).toHaveCount(0);
  await expect.poll(() => engine.sent.some((m) => m.picture === true)).toBe(true);
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_prefs.v1") ?? "{}").pictureHere)).toBe(false);
});

test("a page that comes back to a session with the picture off shows it off", async ({ page }) => {
  const engine = await fakeEngine(page, { host: "192.168.1.100", playing: true, picture: false });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  const show = page.getByRole("button", { name: "Show it here" });
  await expect(show).toBeVisible({ timeout: 15_000 });
  await show.click();
  await expect.poll(() => engine.sent.find((m) => "host" in m)?.attach_only).toBe(true);
  expect(engine.sent.find((m) => "host" in m)?.picture).toBeUndefined();
  engine.socket.ws?.send(JSON.stringify({ picture: false }));
  await expect(page.getByTestId("rp-picture-off")).toBeVisible();
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  test("the menu turns it off, the placeholder fits, and the on-screen controller goes", async ({ page }) => {
    const engine = await fakeEngine(page);
    await start(page);
    await page.getByTestId("rp-more").click();
    await page.getByTestId("rp-menu-picture").click();
    const ph = page.getByTestId("rp-picture-off");
    await expect(ph).toBeVisible();
    await expect.poll(() => engine.sent.some((m) => m.picture === false)).toBe(true);
    const box = (await ph.boundingBox())!;
    expect(box.width).toBeLessThanOrEqual(390);
    await expect(page.getByTestId("rp-touch-in-full")).toHaveCount(0);
    await page.getByTestId("rp-picture-on").click();
    await expect(ph).toHaveCount(0);
    await expect.poll(() => engine.sent.some((m) => m.picture === true)).toBe(true);
  });
});
