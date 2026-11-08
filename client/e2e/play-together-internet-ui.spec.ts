import { expect, test, type Page } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// Play Together's internet invites, the host's side (the app): Internet beside "This network" on
// the invite card, the internet link and its QR code, the refusals, and an internet friend in the
// guest list. The engine is faked as in play-together.spec.ts (join.rs and join_net's answers,
// field for field); play-together-internet.spec.ts runs the real hub and WebRTC.

const KEY = "00112233445566778899aabbccddeeff";
const LINK = `http://192.168.1.9:19113/join#i=${KEY}&n=Ann`;
const NET_URL = `https://ps5upload.phantomptr.com/join/#room=r00m&k=${KEY}&fp=AB:CD:EF&hub=hub.phantomptr.com`;

type Phase = "lan" | "internet" | "connecting" | "playing";

/** The engine's answers while `phase()` says where the invite is; `posts` gathers what was sent. */
async function fakeEngine(page: Page, phase: () => Phase, internetInvites = true) {
  const posts: Array<{ path: string; body: Record<string, unknown> }> = [];
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "11.6.0", caps: {} };
    if (path === "/api/ps5/rp/session") return { host: null };
    if (!path.startsWith("/api/join")) return {};
    if (req.method() === "POST") posts.push({ path, body: req.postDataJSON() as Record<string, unknown> });
    const p = phase();
    const net = p !== "lan";
    const peers =
      p === "connecting"
        ? [{ gid: "g1", path: "connecting", ice: "checking", rtt_ms: null, since_s: 2, why: null }]
        : p === "playing"
          ? [{ gid: "g1", path: "direct", ice: "connected", rtt_ms: 35, since_s: 9, why: null }]
          : [];
    const invite = {
      key: KEY,
      code: "K7M2QX",
      code_display: "K7M-2QX",
      url: LINK,
      urls: [LINK],
      expires_in_s: 1800,
      internet: net
        ? { url: NET_URL, room: "r00m", hub: "hub.phantomptr.com", fp: "AB:CD:EF", state: "hosted", error: null, router: null, peers }
        : null,
    };
    const whole = {
      open: true,
      console: "192.168.1.100",
      playing: true,
      host_away_s: null,
      invite,
      players_in: p === "playing" ? 1 : 0,
      guests:
        p === "playing"
          ? [
              {
                id: 4, name: "Ben", player: 2, state: "playing", connected: true, reconnect_left_s: null, rtt_ms: 35,
                queue: 0, skipped: 0, since_s: 5, why: null, stream_seat: false, via: "internet", path: "direct", ice_rtt_ms: 33,
              },
            ]
          : [],
      pending: [],
      settings: {
        block_system_buttons: true,
        max_players: 3,
        stream_seat: false,
        internet_invites: internetInvites,
        hub_url: "wss://hub.phantomptr.com/ws",
        stun: ["stun.cloudflare.com:3478", "stun.l.google.com:19302"],
        upnp: true,
      },
      max_guests: 3,
      internet: net ? { room: "r00m", peers } : null,
      ws: "ws://192.168.1.50:19113/api/join/ws",
    };
    if (path === "/api/join/invite") return invite;
    if (path === "/api/join/settings") return { settings: whole.settings };
    return whole;
  });
  // Off in the settings: the engine refuses an internet invite (409 internet_off).
  if (!internetInvites) {
    await page.route(
      (url) => url.pathname === "/api/join/invite",
      async (route) => {
        const body = route.request().postDataJSON() as Record<string, unknown>;
        posts.push({ path: "/api/join/invite", body });
        if (body.internet) await route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: "internet_off" }) });
        else await route.fallback();
      },
    );
  }
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if ((JSON.parse(String(m)) as { host?: string }).host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });
  return posts;
}

async function openPlayTogether(page: Page) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-together").click();
  return page.getByTestId("pt-panel");
}

test("Internet: the link and QR code, then a friend listed as Internet · Direct", async ({ page }) => {
  await seedConsole(page);
  let phase: Phase = "lan";
  const posts = await fakeEngine(page, () => phase);
  const panel = await openPlayTogether(page);

  // This network first: the LAN link, Internet offered beside it.
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK);
  await expect(panel.getByTestId("pt-addresses-home")).toHaveAttribute("aria-pressed", "true");

  // Internet: the engine is asked for an internet invite, and its link replaces the LAN one.
  phase = "internet";
  await panel.getByTestId("pt-addresses-internet").click();
  await expect.poll(() => posts.find((p) => p.path === "/api/join/invite" && p.body.internet)?.body).toEqual({
    addr: "192.168.1.100",
    internet: true,
  });
  await expect(panel.getByTestId("pt-url")).toHaveText(NET_URL);
  await expect(panel.getByTestId("pt-qr")).toBeVisible();
  await expect(panel.getByTestId("pt-addresses-internet")).toHaveAttribute("aria-pressed", "true");
  await expect(panel.getByTestId("pt-addresses-internet-hint")).toContainText("Works from anywhere");
  // The short code is for the same network only.
  await expect(panel.getByTestId("pt-code")).toHaveCount(0);
  // Remembered on this device.
  expect(await page.evaluate(() => window.localStorage.getItem("ps5upload.pt_network"))).toBe("internet");

  // A friend's browser connecting.
  phase = "connecting";
  await expect(panel.getByTestId("pt-net-peer")).toContainText("Connecting…", { timeout: 10_000 });

  // In the game: Internet · Direct, with the delay.
  phase = "playing";
  const card = panel.getByTestId("pt-player-2");
  await expect(card.getByTestId("pt-player-name")).toHaveText("Ben", { timeout: 10_000 });
  await expect(card.getByTestId("pt-player-via")).toHaveText("Internet · Direct");
  await expect(card.getByTestId("pt-player-delay")).toHaveText("35 ms");
  await expect(panel.getByTestId("pt-net-peer")).toHaveCount(0);

  // Back to this network: the LAN link again.
  await panel.getByTestId("pt-addresses-home").click();
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK);
});

test("Internet invites off in the settings: said, and the LAN link still works", async ({ page }) => {
  await seedConsole(page);
  await fakeEngine(page, () => "lan", false);
  const panel = await openPlayTogether(page);
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK);
  await panel.getByTestId("pt-addresses-internet").click();
  await expect(panel.getByTestId("pt-net-off")).toContainText("Settings › Play Together");
  await expect(panel.getByTestId("pt-url")).toHaveCount(0);
  await panel.getByTestId("pt-addresses-home").click();
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK);
  await expect(panel.getByTestId("pt-net-off")).toHaveCount(0);
});
