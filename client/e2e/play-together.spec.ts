import { readFile } from "node:fs/promises";
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

// Play Together (#641): friends play the host's PS5 as Player 2–4. The host opens it from the
// stream bar (QR, Allow, Remove); a friend's browser opens /join (the engine's join_page.html,
// npm run build:join) and plays through /api/join/ws; the app joins from Play › Join a friend
// through its own engine (/api/join/relay). A fake engine on every side; everything muted
// (playwright.config.ts --mute-audio).

const INVITE = "00112233445566778899aabbccddeeff";
const LINK = `http://192.168.1.9:19113/join#i=${INVITE}&n=Ann`;

async function consoleSetUp(page: Page) {
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
}

/** A standard-mapping controller the test presses: window.__pad.buttons[i] = 1. */
async function fakeController(page: Page) {
  await page.addInitScript(() => {
    const state = { buttons: new Array(17).fill(0) as number[], axes: [0, 0, 0, 0] };
    (window as unknown as { __pad: typeof state }).__pad = state;
    navigator.getGamepads = () =>
      [
        {
          id: "Wireless Controller (STANDARD GAMEPAD Vendor: 054c Product: 0ce6)",
          index: 0,
          connected: true,
          mapping: "standard",
          timestamp: performance.now(),
          axes: [...state.axes],
          buttons: state.buttons.map((v) => ({ pressed: v > 0.5, touched: v > 0, value: v })),
          vibrationActuator: null,
        },
      ] as unknown as (Gamepad | null)[];
  });
}

const press = (page: Page, i: number, v: number) =>
  page.evaluate(([i, v]) => ((window as unknown as { __pad: { buttons: number[] } }).__pad.buttons[i] = v), [i, v]);

test("the host invites, allows a friend, sees their delay and removes them", async ({ page }) => {
  await consoleSetUp(page);
  const posts: Array<{ path: string; body: Record<string, unknown> }> = [];
  let phase: "open" | "asked" | "playing" | "kicked" = "open";
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.8.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path.startsWith("/api/join")) {
        if (req.method() === "POST") {
          const b = req.postDataJSON() as Record<string, unknown>;
          posts.push({ path, body: b });
          if (path === "/api/join/decide") phase = "playing";
          if (path === "/api/join/kick") phase = "kicked";
        }
        body = {
          open: true,
          invite: { url: LINK, code: "K7M2QX", expires_ms: 1_800_000, urls: [LINK, `http://100.64.0.7:19113/join#i=${INVITE}&n=Ann`] },
          guests:
            phase === "playing"
              ? [{ id: 4, name: "Ben", player: 2, status: "playing", rtt_ms: 180, connected: true }]
              : phase === "kicked"
                ? [{ id: 4, name: "Ben", player: 2, status: "left", rtt_ms: null, connected: false }]
                : [],
          asks: phase === "asked" ? [{ id: 4, name: "Ben" }] : [],
          settings: { block_system_buttons: true, max_players: 3 },
          helper_ok: true,
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
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-together").click();

  // ① Invite: the QR code, the link and the other address.
  const panel = page.getByTestId("pt-panel");
  await expect(panel.getByTestId("pt-qr")).toBeVisible();
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK);
  await expect.poll(() => posts.map((p) => p.path)).toContain("/api/join/open");
  expect(posts.find((p) => p.path === "/api/join/open")?.body).toEqual({ addr: "192.168.1.100" });
  await panel.getByTestId("pt-address").nth(1).click();
  await expect(panel.getByTestId("pt-url")).toHaveText(`http://100.64.0.7:19113/join#i=${INVITE}&n=Ann`);
  await expect(panel.getByTestId("pt-code")).toHaveText("K7M-2QX");
  await expect(panel.getByTestId("pt-tailscale")).toContainText("Tailscale");
  await expect(panel.getByTestId("pt-player-2")).toContainText("Free");

  // ② A friend asks: Allow over the picture.
  phase = "asked";
  await expect(page.getByTestId("pt-ask")).toContainText("Ben wants to play with you", { timeout: 10_000 });
  await page.getByTestId("pt-allow").click();
  await expect.poll(() => posts.find((p) => p.path === "/api/join/decide")?.body).toEqual({ addr: "192.168.1.100", id: 4, allow: true });

  // Playing: name, status, a slow delay in red, and the advice.
  const card = panel.getByTestId("pt-player-2");
  await expect(card.getByTestId("pt-player-name")).toHaveText("Ben", { timeout: 10_000 });
  await expect(card.getByTestId("pt-player-status")).toHaveText("Playing");
  await expect(card.getByTestId("pt-player-delay")).toHaveAttribute("data-tone", "slow");
  await expect(panel.getByTestId("pt-slow")).toContainText("Ben's connection is slow (180 ms)");

  // Stopping Play with a friend in asks first; No keeps playing.
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-toolbar").getByTestId("rp-stop").click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toContainText("Stopping Play ends Play Together for 1 friend(s). Continue?");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByTestId("rp-toolbar")).toBeAttached();

  // Block PS & Create is a setting the engine keeps.
  await panel.getByTestId("pt-block").getByRole("switch").click();
  await expect.poll(() => posts.find((p) => p.path === "/api/join/settings")?.body).toEqual({
    addr: "192.168.1.100",
    block_system_buttons: false,
  });

  await card.getByTestId("pt-kick").click();
  await expect.poll(() => posts.find((p) => p.path === "/api/join/kick")?.body).toEqual({ addr: "192.168.1.100", player: 2 });
  await expect(card.getByTestId("pt-player-status")).toHaveText("Left", { timeout: 10_000 });

  // The phones panel is one click away.
  await panel.getByTestId("pt-phones").click();
  await expect(page.getByTestId("rp-phone-pads")).toBeVisible();
});

test("an old helper is pointed at the update", async ({ page }) => {
  await consoleSetUp(page);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.8.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path.startsWith("/api/join")) body = { open: false, invite: null, guests: [], asks: [], helper_ok: false, reason: "old_helper" };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if ((JSON.parse(String(m)) as { host?: string }).host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await page.getByTestId("rp-start").click({ timeout: 30_000 });
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-together").click();
  await expect(page.getByTestId("pt-helper-old")).toContainText("Update the helper to play together");
  await expect(page.getByTestId("pt-qr")).toHaveCount(0);
});

/** The guest page as the engine serves it. */
async function serveJoinPage(page: Page) {
  const html = await readFile(new URL("../../engine/crates/ps5upload-engine/src/join_page.html", import.meta.url), "utf8");
  await page.route("**/join", (route) => route.fulfill({ contentType: "text/html", body: html }));
}

/** One second of 320x240 H.264 as the engine forwards it (kind 0 header, kind 1 frames). */
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

test("a friend's browser joins, plays as Player 2, and survives a dropped connection", async ({ page }) => {
  await fakeController(page);
  await serveJoinPage(page);
  const hellos: Array<Record<string, unknown>> = [];
  const frames: number[][] = [];
  const sockets: WebSocketRoute[] = [];
  let stream: number[][] = [];
  let allow: () => void = () => {};
  await page.routeWebSocket(/\/api\/join\/ws/, (ws) => {
    sockets.push(ws);
    ws.onMessage((m) => {
      if (typeof m !== "string") {
        frames.push([...m]);
        return;
      }
      const v = JSON.parse(m) as { hello?: Record<string, unknown>; ping?: number };
      if (v.ping !== undefined) ws.send(JSON.stringify({ pong: v.ping }));
      if (!v.hello) return;
      hellos.push(v.hello);
      const joined = () => {
        ws.send(JSON.stringify({ stage: "joined", player: 2, token: "TOKEN-1", hevc: false }));
        ws.send(JSON.stringify({ status: { player: 2, state: "picking_user", rtt_ms: 12, host: "Ann", block_system: true } }));
        for (const f of stream) ws.send(Buffer.from(f));
      };
      // A resume is answered a moment later, so the Reconnecting overlay shows meanwhile.
      if (v.hello.resume === "TOKEN-1") setTimeout(joined, 1500);
      else {
        ws.send(JSON.stringify({ stage: "asking" }));
        allow = joined;
      }
    });
  });

  await page.goto("/", { waitUntil: "domcontentloaded" });
  stream = await makeStream(page);
  await page.goto(`/join#i=${INVITE}&n=Ann`, { waitUntil: "domcontentloaded" });
  await page.getByTestId("join-name").fill("Ben");
  await page.getByTestId("join-go").click();
  await expect(page.getByTestId("join-stage")).toHaveText("Waiting for Ann to let you in", { timeout: 30_000 });
  expect(hellos[0]).toEqual({ name: "Ben", invite: INVITE });

  allow();
  await expect(page.getByTestId("join-player")).toHaveText("You are Player 2");
  await expect(page.getByTestId("join-picking")).toHaveText("On the TV: pick Quick Play");
  await expect(page.getByTestId("join-guide")).toContainText("Press PS on your controller");
  await expect(page.getByTestId("join-pad")).toHaveText("Your controller is ready");
  await expect.poll(() => page.getByTestId("watch-canvas").evaluate((c) => (c as HTMLCanvasElement).width)).toBe(320);
  await expect(page.getByTestId("join-delay")).toBeVisible({ timeout: 5_000 });

  // Cross held: the exact frame, then keepalives at least every 100 ms.
  await press(page, 0, 1);
  await expect.poll(() => frames.some((f) => f[5] === 1)).toBe(true);
  const f = frames.find((x) => x[5] === 1)!;
  expect(f[0]).toBe(0x01);
  expect(f.slice(5)).toEqual([1, 0, 0, 0, 128, 128, 128, 128, 0, 0, 0]);
  const n = frames.length;
  await page.waitForTimeout(600);
  expect(frames.length - n).toBeGreaterThanOrEqual(4);
  const seqs = frames.map((x) => new DataView(Uint8Array.from(x).buffer).getUint32(1, true));
  for (let i = 1; i < seqs.length; i++) expect(seqs[i]).toBeGreaterThan(seqs[i - 1]);
  await press(page, 0, 0);

  // The network drops: Reconnecting, then back in as Player 2 with the token, no new ask.
  await sockets[0].close();
  await expect(page.getByTestId("join-reconnecting")).toBeVisible();
  await expect(page.getByTestId("join-reconnecting-left")).toContainText("Your place is kept for 0:");
  await expect(page.getByTestId("join-reconnecting")).toHaveCount(0, { timeout: 10_000 });
  expect(hellos[hellos.length - 1]).toEqual({ name: "Ben", invite: INVITE, resume: "TOKEN-1" });
  await expect(page.getByTestId("join-player")).toHaveText("You are Player 2");

  // Leave says so.
  const last = sockets[sockets.length - 1];
  const left = new Promise<void>((resolve) =>
    last.onMessage((m) => {
      if (typeof m === "string" && m.includes('"leave"')) resolve();
    }),
  );
  await page.getByTestId("join-leave").click();
  await left;
  await expect(page.getByTestId("join-name-step")).toBeVisible();
});

test.describe("a friend's phone", () => {
  test.use({ viewport: { width: 844, height: 390 }, hasTouch: true, isMobile: true });

  test("plays with the on-screen controller", async ({ page }) => {
    await serveJoinPage(page);
    const frames: number[][] = [];
    await page.routeWebSocket(/\/api\/join\/ws/, (ws) => {
      ws.onMessage((m) => {
        if (typeof m !== "string") return void frames.push([...m]);
        if (m.includes("hello")) ws.send(JSON.stringify({ stage: "joined", player: 3, token: "T3", hevc: false }));
      });
    });
    await page.goto(`/join#i=${INVITE}`, { waitUntil: "domcontentloaded" });
    await page.getByTestId("join-name").fill("Cy");
    await page.getByTestId("join-go").tap();
    await expect(page.getByTestId("join-player")).toHaveText("You are Player 3", { timeout: 30_000 });
    await page.getByTestId("join-guide-ok").tap();
    await page.getByRole("button", { name: "✕" }).tap();
    await expect.poll(() => frames.some((f) => f[5] === 1)).toBe(true);
  });
});

test("a friend is told why they can't play, and can ask again", async ({ page }) => {
  await serveJoinPage(page);
  const answers = ["not_playing", "full", "denied", "key"];
  const texts: Record<string, string> = {
    not_playing: "Ann isn't playing right now. Ask Ann to start Play first.",
    full: "The game is full: four players already.",
    denied: "Ann didn't let you in this time.",
    key: "This invite has expired. Ask Ann for a new link.",
  };
  let asked = 0;
  await page.routeWebSocket(/\/api\/join\/ws/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m === "string" && m.includes("hello")) ws.send(JSON.stringify({ stage: "refused", why: answers[asked++] }));
    });
  });
  await page.goto(`/join#i=${INVITE}&n=Ann`, { waitUntil: "domcontentloaded" });
  await page.getByTestId("join-name").fill("Ben");
  await page.getByTestId("join-go").click();
  for (let i = 0; i < answers.length; i++) {
    await expect(page.getByTestId("join-ended")).toHaveAttribute("data-why", answers[i], { timeout: 30_000 });
    await expect(page.getByTestId("join-ended-text")).toHaveText(texts[answers[i]]);
    if (i === 0) await expect(page.getByTestId("join-again")).toHaveText("Try again");
    if (i + 1 < answers.length) await page.getByTestId("join-again").click();
  }
});

test.describe("a friend whose browser speaks German", () => {
  test.use({ locale: "de-DE" });

  test("gets the page in German", async ({ page }) => {
    await serveJoinPage(page);
    await page.goto(`/join#i=${INVITE}&n=Ann`, { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Bei Freunden mitspielen" })).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId("join-go")).toHaveText("Mitspielen");
    await expect(page.locator("html")).toHaveAttribute("lang", "de");
  });
});

test("the host stopping keeps the friend's place until they play again", async ({ page }) => {
  await serveJoinPage(page);
  let bye: () => void = () => {};
  let back: () => void = () => {};
  const hellos: Array<Record<string, unknown>> = [];
  await page.routeWebSocket(/\/api\/join\/ws/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m !== "string" || !m.includes("hello")) return;
      const hello = (JSON.parse(m) as { hello: Record<string, unknown> }).hello;
      hellos.push(hello);
      if (hello.resume === "T") {
        // The host's engine holds a resume until they play again.
        ws.send(JSON.stringify({ stage: "waiting", why: "host_stopped" }));
        back = () => ws.send(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false }));
        return;
      }
      ws.send(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false }));
      bye = () => ws.send(JSON.stringify({ bye: "host_stopped" }));
    });
  });
  await page.goto(`/join#i=${INVITE}&n=Ann`, { waitUntil: "domcontentloaded" });
  await page.getByTestId("join-name").fill("Ben");
  await page.getByTestId("join-go").click();
  await expect(page.getByTestId("join-player")).toBeVisible({ timeout: 30_000 });
  bye();
  await expect(page.getByTestId("join-host-away")).toContainText(
    /The host stopped playing — waiting for them to start again \(0:[0-5]\d\)/,
  );
  await expect.poll(() => hellos.length).toBe(2);
  expect(hellos[1].resume).toBe("T");
  back();
  await expect(page.getByTestId("join-host-away")).toHaveCount(0);
  await expect(page.getByTestId("join-player")).toBeVisible();
});

test("the app joins a friend from Play, and asks before starting its own Play", async ({ page }) => {
  await consoleSetUp(page);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.8.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  let relayUrl = "";
  const hellos: Array<Record<string, unknown>> = [];
  await page.routeWebSocket(/\/api\/join\/relay/, (ws) => {
    relayUrl = ws.url();
    ws.onMessage((m) => {
      if (typeof m === "string" && m.includes("hello")) {
        hellos.push((JSON.parse(m) as { hello: Record<string, unknown> }).hello);
        ws.send(JSON.stringify({ stage: "joined", player: 2, token: "APP", hevc: false }));
      }
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await page.getByRole("radio", { name: "Join a friend" }).click({ timeout: 30_000 });
  await page.getByTestId("join-link").fill("http://192.168.1.9:19113/join#i=bad");
  await page.getByTestId("join-app-name").fill("Ben");
  await page.getByTestId("join-app-go").click();
  await expect(page.getByText("That isn't a Play Together link.")).toBeVisible();
  await page.getByTestId("join-link").fill(LINK);
  await page.getByTestId("join-app-go").click();
  await expect(page.getByTestId("join-player")).toHaveText("Playing at Ann's · You are Player 2", { timeout: 30_000 });
  expect(decodeURIComponent(relayUrl)).toContain("url=ws://192.168.1.9:19113/api/join/ws");
  expect(hellos[0]).toEqual({ name: "Ben", invite: INVITE });

  // One thing at a time: starting Play asks to leave Ann's game first.
  await page.getByRole("radio", { name: "Play my PS5" }).click();
  await page.getByTestId("rp-start").click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toContainText("Starting Play leaves Ann's game. Continue?");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await page.getByRole("radio", { name: "Join a friend" }).click();
  await expect(page.getByTestId("join-player")).toBeVisible();
});
