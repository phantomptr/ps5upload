import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

// Remote Play belongs to the app, not to its screen (8.10): leaving the screen keeps the
// stream, a dropped socket rejoins, and only Stop ends it, with no "connection dropped".

test("Remote Play survives leaving the screen and a dropped socket", async ({ page }) => {
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
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "8.10.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const starts: Array<Record<string, unknown>> = [];
  let stops = 0;
  let socket: WebSocketRoute | null = null;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    socket = ws;
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
  const toolbar = page.getByTestId("rp-toolbar");
  await expect(toolbar).toContainText("Living room");
  // The bar shows before the routed socket has handed the start message to the test.
  await expect.poll(() => starts.length).toBeGreaterThan(0);
  expect(starts[0]).toMatchObject({ host: "192.168.1.100", attach_only: false });

  // Stats on the picture are a toggle.
  const statsToggle = page.getByTestId("rp-toggle-stats");
  await expect(statsToggle).toHaveAttribute("aria-pressed", "false");
  await statsToggle.click();
  await expect(statsToggle).toHaveAttribute("aria-pressed", "true");

  // Volume is on the picture's bar, not only in Settings.
  await page.getByTestId("rp-volume").fill("40");
  await expect
    .poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_prefs.v1") ?? "{}").volume))
    .toBe(0.4);

  // Another screen: the stream keeps going, and a bar leads back.
  await page.evaluate(() => {
    window.history.pushState({}, "", "/games");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  const banner = page.getByTestId("rp-banner");
  await expect(banner).toContainText("Living room");
  await banner.getByRole("button", { name: "Return to it" }).click();
  await expect(toolbar).toBeVisible();
  expect(stops).toBe(0);

  // The engine socket drops: the page rejoins the running session on its own.
  await socket!.close({ code: 1006 });
  await expect.poll(() => starts.length, { timeout: 15_000 }).toBe(2);
  expect(starts[1]).toMatchObject({ attach_only: true });
  await expect(page.getByText("The connection to the engine dropped.")).toHaveCount(0);

  // Stop ends it cleanly: no error.
  await toolbar.hover();
  await page.getByTestId("rp-stop").click();
  await expect.poll(() => stops).toBe(1);
  await expect(page.getByTestId("rp-start")).toBeVisible();
  await expect(page.locator("[role=alert]").filter({ hasText: /engine/ })).toHaveCount(0);
});

// Two consoles: the page follows the console selected in the app. The other console's stream
// keeps going and is offered there, and Start on this one stops it first.
test("Remote Play shows the selected console while another one streams", async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    const p = (id: string, name: string, host: string) => ({
      id,
      name,
      host,
      wake_regist_key: "00112233",
      wake_rp_key: "00112233445566778899aabbccddeeff",
    });
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [p("p1", "Living room", "192.168.1.100"), p("p2", "Bedroom", "192.168.1.101")],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "8.11.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const starts: Array<Record<string, unknown>> = [];
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
  const visible = (id: string) => page.getByTestId(id).filter({ visible: true });
  await expect(visible("rp-toolbar")).toContainText("Living room");

  // Select the other console in the app: its own Start, with the running stream offered.
  await page.getByRole("button", { name: /Bedroom/ }).first().click();
  await expect(page.getByText("Play is streaming Living room")).toBeVisible();
  // Each console keeps its own (hidden) copy of the screen: only the shown one counts.
  await expect(visible("rp-toolbar")).toHaveCount(0);
  const start = visible("rp-start");
  await expect(start).toContainText("Stop the other stream and start");

  await start.click();
  await expect.poll(() => starts.length, { timeout: 15_000 }).toBe(2);
  expect(stops).toBe(1);
  expect(starts[1]).toMatchObject({ host: "192.168.1.101" });
  await expect(visible("rp-toolbar")).toContainText("Bedroom");
});

// Picture in picture: the stream's canvas moves into a small window of its own and back. The
// browser's Document Picture-in-Picture is stood in for by a plain popup (same kind of window:
// same origin, scriptable), which headless Chromium can open without a real PiP window.
test("Remote Play moves into picture in picture and back", async ({ page }) => {
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
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
    Object.defineProperty(window, "documentPictureInPicture", {
      configurable: true,
      value: {
        requestWindow: async (o: { width: number; height: number }) => {
          const w = window.open("", "_blank", `width=${o.width},height=${o.height}`);
          if (!w) throw new Error("popup blocked");
          return w;
        },
      },
    });
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "8.11.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  let stops = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) ws.send(JSON.stringify({ stage: "first_frame" }));
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
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  await expect(stage.getByTestId("rp-canvas")).toHaveCount(1);

  // Into picture in picture: the canvas leaves the page for the small window.
  const popupP = page.waitForEvent("popup");
  await page.getByTestId("rp-pip").click();
  const popup = await popupP;
  await expect(popup.getByTestId("rp-pip-player")).toBeVisible();
  await expect(popup.getByTestId("rp-canvas")).toHaveCount(1);
  await expect(popup).toHaveTitle("Living room · Play");
  // The whole picture in the small window, whatever its shape: a 1080p picture, 16:9, inside it.
  const popCanvas = popup.getByTestId("rp-canvas");
  // Sized against the window's picture box, never a viewport the browser picks (#566).
  expect(await popCanvas.evaluate((c) => getComputedStyle(c.parentElement!).containerType)).toBe("size");
  await popCanvas.evaluate((c: HTMLCanvasElement) => {
    c.width = 1920;
    c.height = 1080;
    c.style.setProperty("--rp-shape", String(1920 / 1080));
  });
  for (const [w, h] of [
    [640, 480],
    [900, 380],
  ]) {
    await popup.setViewportSize({ width: w, height: h });
    await expect
      .poll(() =>
        popCanvas.evaluate((c) => {
          const r = c.getBoundingClientRect();
          const inside = r.left >= -0.5 && r.top >= -0.5 && r.right <= innerWidth + 0.5 && r.bottom <= innerHeight + 0.5;
          const shape = Math.abs(r.width / r.height - 16 / 9) < 0.01;
          const full = Math.abs(r.width - innerWidth) < 1 || Math.abs(r.height - innerHeight) < 1;
          return inside && shape && full;
        }),
      )
      .toBe(true);
  }
  await expect(stage.getByTestId("rp-pip-on")).toBeVisible();
  await expect(stage.getByTestId("rp-canvas")).toHaveCount(0);
  await expect(page.getByTestId("rp-pip")).toHaveAttribute("aria-pressed", "true");

  // Back to the app: the window closes and the picture is here again, still playing.
  await popup.getByTestId("rp-pip-player").hover();
  await popup.getByTestId("rp-pip-back").click();
  await expect(stage.getByTestId("rp-canvas")).toHaveCount(1);
  await expect(stage.getByTestId("rp-pip-on")).toHaveCount(0);
  await expect.poll(() => popup.isClosed()).toBe(true);

  // Closed from outside (its own close button): the picture comes back the same way.
  const popup2P = page.waitForEvent("popup");
  await page.getByTestId("rp-pip").click();
  const popup2 = await popup2P;
  await expect(popup2.getByTestId("rp-canvas")).toHaveCount(1);
  await popup2.close();
  await expect(stage.getByTestId("rp-canvas")).toHaveCount(1);
  await expect(stage.getByTestId("rp-pip-on")).toHaveCount(0);

  // Stop in the small window ends the stream and closes it.
  const popup3P = page.waitForEvent("popup");
  await page.getByTestId("rp-pip").click();
  const popup3 = await popup3P;
  await popup3.getByTestId("rp-pip-player").hover();
  await popup3.getByTestId("rp-pip-stop").click();
  await expect.poll(() => stops).toBe(1);
  await expect.poll(() => popup3.isClosed()).toBe(true);
  await expect(page.getByTestId("rp-start").filter({ visible: true })).toBeVisible();
});

// Esc in full screen: a quick press is Circle on the console, holding it leaves full screen
// (without pressing Circle).
test("Remote Play: Esc is Circle in full screen, held it leaves", async ({ page }) => {
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
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "8.10.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const pads: number[] = [];
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; pad?: { buttons: number } };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.pad) pads.push(msg.pad.buttons);
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  const stage = page.getByTestId("rp-stage");
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-fullscreen").click();
  await expect(stage).toHaveAttribute("data-full", "bar");
  await stage.focus();

  // A quick press: Circle (2) down and up, and still full screen.
  await page.keyboard.press("Escape");
  await expect.poll(() => pads.includes(2)).toBe(true);
  await expect.poll(() => pads[pads.length - 1]).toBe(0);
  await expect(stage).toHaveAttribute("data-full", "bar");

  // Held: leaves full screen, and Circle is not pressed again.
  const before = pads.filter((b) => b & 2).length;
  await page.keyboard.down("Escape");
  await expect(stage).not.toHaveAttribute("data-full", "bar", { timeout: 3_000 });
  await page.keyboard.up("Escape");
  await page.waitForTimeout(300);
  expect(pads.filter((b) => b & 2).length).toBe(before);
});

// #548: a Linux webview without GStreamer's H.264 decoder. The app finds out before it wakes the
// console, ends cleanly, and says what to install.
test("Remote Play without an H.264 decoder says what to install and never starts", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "userAgent", {
      get: () => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)",
    });
    VideoDecoder.isConfigSupported = async (config: VideoDecoderConfig) => ({ supported: false, config });
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
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.4.1", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  let sockets = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, () => {
    sockets++;
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByText("This device has no H.264 video decoder")).toBeVisible();
  await expect(page.getByText("sudo pacman -S gst-libav", { exact: false })).toBeVisible();
  await expect(page.getByTestId("rp-start")).toBeVisible();
  expect(sockets).toBe(0);
});

/** One console set up for Wake & sign in, an engine that answers, and a stream that plays. */
async function playableConsole(page: Page) {
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
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.5.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });
}

// The picture size shows only in full screen (on the page the box is the picture's own shape), so
// it changes there, without leaving full screen, and the picture follows at once.
test("Remote Play: picture size changes in full screen and applies there", async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 1000 }); // 16:10, not the picture's 16:9
  await playableConsole(page);
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  const stage = page.getByTestId("rp-stage");
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-fullscreen").click();
  await expect(stage).toHaveAttribute("data-full", "bar");
  // As where object-fit on a canvas is ignored: each size must still differ, so the stream lays
  // out the canvas for all three.
  await page.addStyleTag({ content: "canvas { object-fit: fill !important }" });

  // A 1080p picture, as the decoder sizes the canvas.
  const canvas = stage.locator("canvas");
  await canvas.evaluate((c: HTMLCanvasElement) => {
    c.width = 1920;
    c.height = 1080;
  });
  const look = () =>
    canvas.evaluate((c: HTMLCanvasElement) => {
      const r = c.getBoundingClientRect();
      return { fit: c.dataset.fit, w: Math.round(r.width), h: Math.round(r.height) };
    });
  // Fit: the canvas is the whole picture's shape inside the screen (bars above and below).
  await expect.poll(look).toEqual({ fit: "fit", w: 1600, h: 900 });

  await page.getByTestId("rp-toolbar").hover();
  const menu = page.getByTestId("rp-fit-menu");
  await expect(menu).toHaveAttribute("aria-label", "Picture size: Fit");
  const pick = async (f: string) => {
    await page.getByTestId("rp-toolbar").hover();
    await menu.click();
    await expect(page.getByTestId(`rp-fit-option-${f}`)).toBeVisible();
    await page.getByTestId(`rp-fit-option-${f}`).click();
    await expect(page.getByTestId("rp-fit-options")).toHaveCount(0); // closes on a pick
  };
  // Fill: the picture's shape over the whole screen, its sides cut.
  await pick("fill");
  await expect.poll(look).toEqual({ fit: "fill", w: 1778, h: 1000 });
  await expect(stage).toHaveAttribute("data-full", "bar"); // still full screen
  // Stretch: the whole screen, the picture squeezed to it.
  await pick("stretch");
  await expect.poll(look).toEqual({ fit: "stretch", w: 1600, h: 1000 });
  // Remembered with the other settings.
  await expect
    .poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_prefs.v1") ?? "{}").fit))
    .toBe("stretch");
  await pick("fit");
  await expect.poll(look).toEqual({ fit: "fit", w: 1600, h: 900 });
  // A press elsewhere closes the menu without changing the size.
  await page.getByTestId("rp-toolbar").hover();
  await menu.click();
  await expect(page.getByTestId("rp-fit-options")).toBeVisible();
  await page.mouse.click(800, 600);
  await expect(page.getByTestId("rp-fit-options")).toHaveCount(0);
  await expect.poll(look).toEqual({ fit: "fit", w: 1600, h: 900 });
});

// The web UI in a browser Remote Play is not made for says which one is, and what this one misses.
test("Remote Play in Firefox names the browser it is made for", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "userAgent", {
      get: () => "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0",
    });
    Object.defineProperty(navigator, "userAgentData", { get: () => undefined });
  });
  await playableConsole(page);
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  const advice = page.getByTestId("rp-browser-advice");
  await expect(advice).toContainText("Play works best in Chrome or Edge");
  await expect(advice).toContainText("In Firefox, Esc always leaves full screen");
  // Dismissed, it stays away.
  await advice.getByRole("button").click();
  await expect(advice).toHaveCount(0);
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("rp-browser-advice")).toHaveCount(0);
});

test("Remote Play in Chrome shows no browser advice", async ({ page }) => {
  await playableConsole(page);
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("rp-start")).toBeVisible();
  await expect(page.getByTestId("rp-browser-advice")).toHaveCount(0);
});

// Screenshots, clips, the latency readout and the two network settings (9.5.0). A real H.264
// frame goes through the app's own decode and draw path, so the screenshot has the stream's own
// size; the engine socket is faked.

const rpFixture = (name: string) =>
  new Uint8Array(readFileSync(fileURLToPath(new URL(`./fixtures/remote-play/${name}`, import.meta.url))));

/** The parameter sets (the "video header") and the picture of a one-frame Annex-B stream. */
function splitH264(stream: Uint8Array): { header: Uint8Array; au: Uint8Array } {
  for (let i = 0; i + 3 < stream.length; i++) {
    if (stream[i] !== 0 || stream[i + 1] !== 0 || stream[i + 2] !== 1) continue;
    const t = stream[i + 3] & 0x1f;
    if (t < 1 || t > 5) continue;
    const start = i > 0 && stream[i - 1] === 0 ? i - 1 : i;
    return { header: stream.slice(0, start), au: stream.slice(start) };
  }
  throw new Error("no picture in the fixture");
}

const rpRecord = (kind: number, payload: Uint8Array) => {
  const b = Buffer.alloc(payload.length + 1);
  b[0] = kind;
  b.set(payload, 1);
  return b;
};

/** The fake engine's clock, µs, as the real one's: monotonic, its own zero. */
const engineUs = () => Math.round(performance.now() * 1000);

/** Kind 7: a video access unit after the engine's u64 BE µs time (for a page that asked). */
const rpTimedVideo = (payload: Uint8Array) => {
  const b = Buffer.alloc(payload.length + 9);
  const t = engineUs();
  b[0] = 7;
  b.writeUInt32BE(Math.floor(t / 4294967296), 1);
  b.writeUInt32BE(t % 4294967296, 5);
  b.set(payload, 9);
  return b;
};

async function rpPage(page: Page, prefs: Record<string, unknown> = {}) {
  await page.addInitScript((p) => {
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
    window.localStorage.setItem("ps5upload.rp_prefs.v1", JSON.stringify(p));
    // H.264 only: the fixtures the fake engine sends.
    if (typeof VideoDecoder !== "undefined") {
      const orig = VideoDecoder.isConfigSupported.bind(VideoDecoder);
      VideoDecoder.isConfigSupported = (cfg: VideoDecoderConfig) =>
        /^(hev1|hvc1)/.test(cfg.codec) ? Promise.resolve({ supported: false, config: cfg }) : orig(cfg);
    }
  }, prefs);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.5.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
}

/** A fake engine that streams the fixture for the size asked for and answers pings. `extra`
 *  runs on every start (its index) for what a test adds: `repeat` sends something every so often
 *  while that socket is open; returning true answers the start with nothing else. `timed`: as
 *  an engine from 10.2 on, its clock in pongs and video as kind 7 to a page that asks; without
 *  it, an older engine (pongs without a clock, video as kind 1). */
async function rpEngine(
  page: Page,
  extra?: (ws: WebSocketRoute, n: number, repeat: (fn: () => void, ms: number) => void) => boolean | void,
  { timed = false }: { timed?: boolean } = {},
) {
  const starts: Array<Record<string, unknown>> = [];
  let pings = 0;
  let stops = 0;
  let keyframes = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    const timers: ReturnType<typeof setInterval>[] = [];
    ws.onClose(() => timers.forEach(clearInterval));
    ws.onMessage((m) => {
      if (typeof m !== "string") return;
      const msg = JSON.parse(m) as Record<string, unknown>;
      if ("ping" in msg) {
        pings++;
        ws.send(JSON.stringify(timed ? { pong: msg.ping, engine_us: engineUs() } : { pong: msg.ping }));
      }
      if (msg.keyframe) keyframes++;
      if (msg.stop) {
        stops++;
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
      if (!("host" in msg)) return;
      starts.push(msg);
      const repeat = (fn: () => void, ms: number) => timers.push(setInterval(fn, ms));
      if (extra?.(ws, starts.length - 1, repeat)) return;
      const { header, au } = splitH264(rpFixture(msg.height === 720 ? "h264_1280x720.h264" : "h264_1920x1080.h264"));
      ws.send(JSON.stringify({ stage: "first_frame" }));
      ws.send(rpRecord(0, header));
      const video = () => (timed && msg.timing ? rpTimedVideo(au) : rpRecord(1, au));
      // The same keyframe again and again: a picture that keeps coming, as a game's does.
      for (let i = 0; i < 3; i++) ws.send(video());
      repeat(() => ws.send(video()), 100);
    });
  });
  return { starts, pings: () => pings, stops: () => stops, keyframes: () => keyframes };
}

test("Remote Play saves a screenshot and a clip, shows its latency and asks for low latency", async ({ page }) => {
  test.setTimeout(90_000);
  await rpPage(page, { res: "1280x720", showStats: true });
  const engine = await rpEngine(page);
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });

  // Low latency is a setting, off until chosen; the start says so to the engine.
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const low = page.getByRole("switch", { name: "Low latency" });
  await expect(low).toHaveAttribute("aria-checked", "false");
  await low.click();
  await expect(page.getByRole("switch", { name: "Lower quality on a busy network" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await page.getByTestId("rp-start").click();
  await expect.poll(() => engine.starts.length).toBe(1);
  expect(engine.starts[0]).toMatchObject({ low_latency: true, height: 720 });

  // The stats measure the round trip to the engine (pinged once a second).
  await expect.poll(() => engine.pings(), { timeout: 10_000 }).toBeGreaterThan(0);
  await expect(page.getByTestId("rp-latency")).toContainText(/Latency ≈ \d+ ms engine \+ \d+ ms decode/, {
    timeout: 10_000,
  });

  // Screenshot: a PNG at the stream's own size, named after the console and the time.
  const toolbar = page.getByTestId("rp-toolbar");
  await toolbar.hover();
  const shotDl = page.waitForEvent("download");
  await page.getByTestId("rp-screenshot").click();
  const shot = await shotDl;
  expect(shot.suggestedFilename()).toMatch(/^PS5 Living room \d{4}-\d\d-\d\d \d\d-\d\d-\d\d\.png$/);
  const png = readFileSync(await shot.path());
  expect(png.subarray(1, 4).toString()).toBe("PNG");
  expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([1280, 720]);
  await expect(page.getByTestId("rp-capture-notice")).toContainText("Screenshot saved");

  // A clip: the recording light counts up; stopping saves it.
  await toolbar.hover();
  await page.getByTestId("rp-record").click();
  await expect(page.getByTestId("rp-recording")).toContainText(/Recording 0:0\d/);
  await expect(page.getByTestId("rp-record")).toHaveAttribute("aria-pressed", "true");
  await page.waitForTimeout(1500);
  const clipDl = page.waitForEvent("download");
  await toolbar.hover();
  await page.getByTestId("rp-record").click();
  const clip = await clipDl;
  expect(clip.suggestedFilename()).toMatch(/^PS5 Living room \d{4}-\d\d-\d\d \d\d-\d\d-\d\d\.(mp4|webm)$/);
  expect(readFileSync(await clip.path()).length).toBeGreaterThan(1000);
  await expect(page.getByTestId("rp-recording")).toHaveCount(0);
  await expect(page.getByTestId("rp-capture-notice")).toContainText("Clip saved");
});

// Latency stages (10.2): the page asks for timed video, lines its clock up with the engine's from
// pings, and shows each stage's median. An older engine has no "net" stage, and the rest still
// shows.
for (const timed of [true, false]) {
  test(`Remote Play times each frame's stages (${timed ? "engine with timed video" : "older engine"})`, async ({
    page,
  }) => {
    test.setTimeout(60_000);
    await rpPage(page, { res: "1280x720", showStats: true });
    const engine = await rpEngine(page, undefined, { timed });
    await page.goto("/play", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
    await page.getByTestId("rp-start").click();
    await expect.poll(() => engine.starts.length).toBe(1);
    expect(engine.starts[0]).toMatchObject({ timing: true });
    const stages = page.getByTestId("rp-latency-stages");
    const net = timed ? "\\d+" : "\\?";
    await expect(stages).toHaveText(new RegExp(`^net ${net} · queue \\d+ · decode \\d+ · screen \\d+ ms$`), {
      timeout: 15_000,
    });
    // The picture is drawn either way.
    await expect(page.getByTestId("rp-latency")).toContainText(/Latency ≈ \d+ ms engine \+ \d+ ms decode/);
  });
}

// The decoder watchdog: frames keep going in, the decoder gives nothing back (and no error).
// It is restarted, a little further apart each time, then the player is told once; it never
// loops.
test("Remote Play restarts a decoder that stopped giving pictures, then gives up", async ({ page }) => {
  test.setTimeout(90_000);
  await page.addInitScript(() => {
    // A decoder that takes every frame and never outputs one.
    VideoDecoder.prototype.decode = function () {};
  });
  await rpPage(page, { res: "1280x720", showStats: true });
  const engine = await rpEngine(page);
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect.poll(() => engine.starts.length).toBe(1);
  const stages = page.getByTestId("rp-latency-stages");
  await expect(stages).toContainText("decoder restarts 1", { timeout: 10_000 });
  expect(engine.keyframes()).toBeGreaterThan(0);
  await expect(stages).toContainText("decoder restarts 3", { timeout: 30_000 });
  await expect(page.getByText("This device could not decode the picture.")).toBeVisible({ timeout: 20_000 });
  // Given up: no more restarts while frames keep coming.
  await page.waitForTimeout(4000);
  await expect(stages).toContainText("decoder restarts 3");
});

test("Remote Play offers 720p when a 1080p stream keeps losing frames, and switches on a tap", async ({ page }) => {
  test.setTimeout(120_000);
  await rpPage(page, { res: "1920x1080" });
  const engine = await rpEngine(page, (ws, n, repeat) => {
    if (n === 0) {
      // A busy network: a tenth of the packets and a few frames lost, every second.
      const lossy = JSON.stringify({
        stage: "stats",
        stats: { frames: 55, frames_rebuilt: 2, frames_lost: 5, video_bytes: 1e6, units: 1000, units_lost: 100 },
      });
      repeat(() => ws.send(lossy), 1000);
    }
    if (n === 1) {
      // The PS5 still holds its slot from the session just stopped.
      ws.send(JSON.stringify({ stage: "error", code: "busy", retry_s: 1, error: "busy" }));
      void ws.close({ code: 1000, reason: "busy" });
      return true;
    }
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect.poll(() => engine.starts.length).toBe(1);
  expect(engine.starts[0]).toMatchObject({ height: 1080, low_latency: false });

  // Not at once: only after the loss has lasted (a few seconds' grace, then about ten).
  await page.waitForTimeout(4000);
  await expect(page.getByTestId("rp-quality-offer")).toHaveCount(0);
  const offer = page.getByTestId("rp-quality-offer");
  await expect(offer).toContainText("The network is dropping frames.", { timeout: 30_000 });
  await offer.getByTestId("rp-quality-switch").click();

  // The stream stops and starts again at 720p, waiting out the PS5's busy answer quietly.
  await expect.poll(() => engine.stops()).toBe(1);
  await expect(page.getByTestId("rp-quality-switching")).toBeVisible();
  await expect.poll(() => engine.starts.length, { timeout: 20_000 }).toBe(3);
  expect(engine.starts[1]).toMatchObject({ height: 720, attach_only: false });
  expect(engine.starts[2]).toMatchObject({ height: 720, attach_only: false });
  await expect(page.getByTestId("rp-quality-switching")).toHaveCount(0, { timeout: 15_000 });
  await expect(page.getByTestId("rp-toolbar")).toContainText("Living room");
  await expect(page.locator("[role=alert]").filter({ hasText: /still finishing/ })).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_prefs.v1") ?? "{}").res))
    .toBe("1280x720");
  // 720p is never offered lower, and nothing steps it back up.
  await page.waitForTimeout(2000);
  await expect(page.getByTestId("rp-quality-offer")).toHaveCount(0);
});

// The microphone (experimental): off until pressed; a press asks for the mic with the browser's
// voice processing on, says {"mic": true}, and sends [5][40-byte Opus] frames; a second press
// stops the track and says {"mic": false}. The mic and the encoder are stand-ins here (no real
// microphone in a test browser); the console side is the engine's, tested in Rust.
test("Remote Play: the microphone sends mic on, Opus frames, and mic off", async ({ page }) => {
  await page.addInitScript(() => {
    // Settings › Microphone (experimental) is on.
    localStorage.setItem("ps5upload.rp_prefs.v1", JSON.stringify({ mic: true }));
    const w = window as unknown as Record<string, unknown>;
    w.__mic = { constraints: null as unknown, stopped: 0 };
    navigator.mediaDevices.getUserMedia = async (c?: MediaStreamConstraints) => {
      (w.__mic as { constraints: unknown }).constraints = c?.audio;
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      const dst = ctx.createMediaStreamDestination();
      osc.connect(dst);
      osc.start();
      const stream = dst.stream;
      for (const t of stream.getTracks()) {
        const stop = t.stop.bind(t);
        t.stop = () => {
          (w.__mic as { stopped: number }).stopped++;
          stop();
        };
      }
      return stream;
    };
    class FakeEncoder {
      state = "unconfigured";
      private out: (c: { byteLength: number; copyTo: (b: Uint8Array) => void }) => void;
      constructor(init: { output: FakeEncoder["out"] }) {
        this.out = init.output;
      }
      static async isConfigSupported(config: Record<string, unknown>) {
        (w.__mic as Record<string, unknown>).config = config;
        return { supported: true, config };
      }
      configure() {
        this.state = "configured";
      }
      encode() {
        // Every frame: one 40-byte packet, like Opus at 32 kbit/s CBR, 10 ms.
        this.out({ byteLength: 40, copyTo: (b) => b.fill(0xab) });
      }
      close() {
        this.state = "closed";
      }
    }
    w.AudioEncoder = FakeEncoder;
  });
  await playableConsole(page);
  const texts: Array<Record<string, unknown>> = [];
  const frames: number[][] = [];
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m !== "string") {
        frames.push([...m.subarray(0, 2), m.length]);
        return;
      }
      const msg = JSON.parse(m) as Record<string, unknown>;
      texts.push(msg);
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-toolbar").hover();
  // The session is set up for the mic only because the setting is on.
  await expect.poll(() => texts.find((t) => t.host)?.mic).toBe(true);
  const mic = page.getByTestId("rp-mic");
  await expect(mic).toHaveAttribute("aria-pressed", "false");
  await expect(mic).toHaveAttribute("title", "Microphone (experimental)");
  expect(texts.some((t) => !t.host && "mic" in t)).toBe(false);

  await mic.click();
  await expect(mic).toHaveAttribute("data-state", "live");
  await expect(mic).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => texts.filter((t) => !t.host && t.mic === true).length).toBe(1);
  await expect.poll(() => frames.length, { timeout: 10_000 }).toBeGreaterThan(3);
  // Kind 5, then the packet: 41 bytes in all.
  expect(frames[0]).toEqual([5, 0xab, 41]);
  const info = await page.evaluate(() => (window as unknown as { __mic: Record<string, unknown> }).__mic);
  expect(info.constraints).toMatchObject({ echoCancellation: true, noiseSuppression: true, autoGainControl: true });
  expect(info.config).toMatchObject({ codec: "opus", sampleRate: 48000, numberOfChannels: 2, bitrate: 32000 });

  await page.getByTestId("rp-toolbar").hover();
  await mic.click();
  await expect(mic).toHaveAttribute("data-state", "off");
  await expect.poll(() => texts.filter((t) => t.mic === false).length).toBe(1);
  expect(await page.evaluate(() => (window as unknown as { __mic: { stopped: number } }).__mic.stopped)).toBeGreaterThan(0);
  // Off means off: no frames after it.
  const n = frames.length;
  await page.waitForTimeout(300);
  expect(frames.length).toBe(n);
});

// Without the setting, a session starts exactly as before the mic existed: no "mic" in the start
// message and no mic button, even where the browser could encode it.
test("Remote Play without the microphone setting has no mic and asks for none", async ({ page }) => {
  await page.addInitScript(() => {
    (window as unknown as Record<string, unknown>).AudioEncoder = {
      isConfigSupported: async () => ({ supported: true }),
    };
  });
  await playableConsole(page);
  const starts: Array<Record<string, unknown>> = [];
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if (msg.host) {
        starts.push(msg);
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
  await expect.poll(() => starts.length).toBe(1);
  expect("mic" in starts[0]).toBe(false);
  await page.getByTestId("rp-toolbar").hover();
  await expect(page.getByTestId("rp-fullscreen")).toBeVisible();
  await expect(page.getByTestId("rp-mic")).toHaveCount(0);
});

// Fit never cuts the picture, whatever shape the screen is: the browser sizes the canvas to the
// whole picture itself, so it holds even where object-fit on a canvas misbehaves (#538) and with a
// scrollbar on the page (100vw counted it, and the right edge went under it).
for (const [w, h] of [
  [1600, 1000],
  [2560, 1080],
  [1280, 1024],
  [1366, 768],
]) {
  test(`Remote Play full screen, Fit shows the whole picture at ${w}x${h}`, async ({ page }) => {
    await page.setViewportSize({ width: w, height: h });
    await rpPage(page, { res: "1920x1080", fit: "fit" });
    await page.addInitScript(() => {
      const style = () => {
        const st = document.createElement("style");
        // As if object-fit on a canvas were ignored, and the page always had a scrollbar.
        st.textContent = "canvas { object-fit: fill !important } html { overflow-y: scroll !important }";
        document.head.appendChild(st);
      };
      if (document.head) style();
      else document.addEventListener("DOMContentLoaded", style);
    });
    await rpEngine(page);
    await page.goto("/play", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
    await page.getByTestId("rp-start").click();
    await expect(page.getByTestId("rp-toolbar")).toBeVisible();
    await page.getByTestId("rp-toolbar").hover();
    await page.getByTestId("rp-fullscreen").click();
    const stage = page.getByTestId("rp-stage");
    await expect(stage).toHaveAttribute("data-full", "bar");
    const canvas = stage.locator("canvas");
    // A real decoded frame: the canvas takes the picture's own size.
    await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width)).toBe(1920);
    const box = () =>
      page.evaluate(() => {
        const c = document.querySelector('[data-testid="rp-stage"] canvas')!.getBoundingClientRect();
        const vw = document.documentElement.clientWidth;
        const vh = document.documentElement.clientHeight;
        return { l: c.left, t: c.top, r: c.right, b: c.bottom, w: c.width, h: c.height, vw, vh };
      });
    await expect
      .poll(async () => {
        const b = await box();
        // Inside the visible area, and the picture's own 16:9 shape (nothing cropped or stretched).
        const inside = b.l >= -0.5 && b.t >= -0.5 && b.r <= b.vw + 0.5 && b.b <= b.vh + 0.5;
        const shape = Math.abs(b.w / b.h - 16 / 9) < 0.01;
        // As big as it can be: touching two opposite sides.
        const full = Math.abs(b.w - b.vw) <= 1 || Math.abs(b.h - b.vh) <= 1;
        return inside && shape && full;
      })
      .toBe(true);
  });
}

test.describe("Remote Play in a phone's browser", () => {
  test.use({
    viewport: { width: 844, height: 390 },
    hasTouch: true,
    isMobile: true,
    userAgent:
      "Mozilla/5.0 (Linux; Android 15; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36",
  });

  test("a screenshot waits for a tap: Save to Photos opens the share sheet, Download downloads", async ({ page }) => {
    test.setTimeout(90_000);
    await page.addInitScript(() => {
      const shared: Array<{ name: string; type: string; size: number }> = [];
      (window as unknown as { __shared: typeof shared }).__shared = shared;
      let first = true;
      Object.defineProperty(navigator, "canShare", { value: (d: ShareData) => !!d.files?.length, configurable: true });
      Object.defineProperty(navigator, "share", {
        configurable: true,
        value: async (d: ShareData) => {
          // The first time the player closes the sheet without choosing.
          if (first) {
            first = false;
            throw new DOMException("closed", "AbortError");
          }
          for (const f of d.files ?? []) shared.push({ name: f.name, type: f.type, size: f.size });
        },
      });
    });
    await rpPage(page, { res: "1280x720", touchControls: "off" });
    await rpEngine(page);
    await page.goto("/play", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });

    // The setting is there on a phone, on by default.
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await expect(page.getByRole("switch", { name: "Save screenshots and clips to Photos" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await page.getByTestId("rp-start").click();
    await expect(page.getByTestId("rp-menu-handle")).toBeVisible({ timeout: 30_000 });

    // Sideways on a phone the bar is behind the handle.
    const shoot = async () => {
      await page.getByTestId("rp-menu-handle").tap();
      await page.getByTestId("rp-screenshot").tap();
    };
    const shared = () => page.evaluate(() => (window as unknown as { __shared: unknown[] }).__shared);

    await shoot();
    const ready = page.getByTestId("rp-capture-ready");
    await expect(ready).toContainText("Screenshot taken");
    // Closing the sheet keeps the notice for another try.
    await page.getByTestId("rp-capture-share").click();
    await expect(ready).toBeVisible();
    await page.getByTestId("rp-capture-share").click();
    await expect.poll(shared).toEqual([expect.objectContaining({ type: "image/png", name: expect.stringMatching(/^PS5 Living room .+\.png$/) })]);
    await expect(ready).toHaveCount(0);

    // Or a download.
    await shoot();
    const dl = page.waitForEvent("download");
    await page.getByTestId("rp-capture-download").click();
    expect((await dl).suggestedFilename()).toMatch(/\.png$/);
    await expect(page.getByTestId("rp-capture-notice")).toContainText("Screenshot saved");
  });
});
