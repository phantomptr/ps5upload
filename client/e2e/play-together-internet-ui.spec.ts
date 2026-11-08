import { expect, test, type Page } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// Play Together's one invite, the host's side (the app): one link and QR code that work anywhere
// (the internet link, which stays on the Wi-Fi for a friend on it), the Wi-Fi's own link while
// there is no internet, Only on my Wi-Fi in the settings, and how each friend is connected. The
// engine is faked as in play-together.spec.ts (join.rs and join_net's answers, field for field);
// play-together-internet.spec.ts runs the real hub and WebRTC.

const KEY = "00112233445566778899aabbccddeeff";
const LINK = `http://192.168.1.9:19113/join#i=${KEY}&n=Ann`;
const NET_URL = `https://ps5upload.phantomptr.com/join/#room=r00m&k=${KEY}&fp=AB:CD:EF&hub=hub.phantomptr.com&lan=http%3A%2F%2F192.168.1.9%3A19113%2Fjoin`;

type Phase = "offline" | "internet" | "connecting" | "playing";

/** The engine's answers while `phase()` says where the invite is; `posts` gathers what was sent. */
async function fakeEngine(page: Page, phase: () => Phase, invites: "anywhere" | "wifi" = "anywhere") {
  const posts: Array<{ path: string; body: Record<string, unknown> }> = [];
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "12.1.0", caps: {} };
    if (path === "/api/ps5/rp/session") return { host: null };
    if (!path.startsWith("/api/join")) return {};
    if (req.method() === "POST") posts.push({ path, body: req.postDataJSON() as Record<string, unknown> });
    const p = phase();
    const anywhere = invites === "anywhere";
    // The room is asked for once the panel asks for the one link (`share`).
    const asked = posts.some((x) => x.path === "/api/join/invite" && x.body.share);
    const net = anywhere && asked;
    const offline = p === "offline";
    const peers =
      p === "connecting"
        ? [{ gid: "g1", path: "connecting", ice: "checking", rtt_ms: null, since_s: 2, why: null }]
        : p === "playing"
          ? [{ gid: "g1", path: "direct", ice: "connected", rtt_ms: 35, since_s: 9, why: null }]
          : [];
    const internet = net
      ? {
          url: NET_URL,
          room: "r00m",
          hub: "hub.phantomptr.com",
          fp: "AB:CD:EF",
          state: offline ? "reconnecting" : "hosted",
          error: offline ? "connect: no route to host" : null,
          offline,
          router: null,
          peers,
        }
      : null;
    const invite = {
      key: KEY,
      code: "K7M2QX",
      code_display: "K7M-2QX",
      url: LINK,
      urls: [LINK],
      expires_in_s: 1800,
      share:
        net && !offline
          ? { url: NET_URL, works: "anywhere", why: null }
          : { url: LINK, works: "wifi", why: !anywhere ? "setting" : net ? "offline" : "local" },
      internet,
    };
    const whole = {
      open: true,
      console: "192.168.1.100",
      playing: true,
      host_away_s: null,
      invite,
      players_in: p === "playing" ? 2 : 0,
      guests:
        p === "playing"
          ? [
              {
                id: 4, name: "Ben", player: 2, state: "playing", connected: true, reconnect_left_s: null, rtt_ms: 35,
                queue: 0, skipped: 0, since_s: 5, why: null, stream_seat: false, via: "internet", path: "direct", ice_rtt_ms: 33,
                link: { path: "internet", rtt_ms: 33 },
              },
              {
                id: 5, name: "Cat", player: 3, state: "playing", connected: true, reconnect_left_s: null, rtt_ms: 6,
                queue: 0, skipped: 0, since_s: 5, why: null, stream_seat: false, via: "internet", path: "direct", ice_rtt_ms: 4,
                link: { path: "lan", rtt_ms: 4 },
              },
            ]
          : [],
      pending: [],
      settings: {
        block_system_buttons: true,
        max_players: 3,
        stream_seat: false,
        internet_invites: anywhere,
        invites,
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

test("one link: the internet one with its QR code, then each friend with how they're connected", async ({ page }) => {
  await seedConsole(page);
  let phase: Phase = "internet";
  const posts = await fakeEngine(page, () => phase);
  const panel = await openPlayTogether(page);

  // The one link, asked for as Invites work says: no Local or Internet to choose.
  await expect.poll(() => posts.find((p) => p.path === "/api/join/invite")?.body).toEqual({ addr: "192.168.1.100", share: true });
  await expect(panel.getByTestId("pt-url")).toHaveText(NET_URL);
  await expect(panel.getByTestId("pt-qr")).toBeVisible();
  await expect(panel.getByTestId("pt-works-anywhere")).toContainText("on your Wi-Fi and over the internet");
  await expect(panel.getByTestId("pt-addresses-home")).toHaveCount(0);
  await expect(panel.getByTestId("pt-addresses-internet")).toHaveCount(0);
  // One quiet line asks for support (no amounts).
  await expect(panel.getByTestId("pt-addresses-internet-support")).toContainText("a donation keeps it going");
  await expect(panel.getByTestId("pt-addresses-internet-support")).not.toContainText(/[$€£]\s?\d/);
  // The short code still works for a friend on the same network.
  await expect(panel.getByTestId("pt-code")).toBeVisible();

  // A friend's browser connecting.
  phase = "connecting";
  await expect(panel.getByTestId("pt-net-peer")).toContainText("Connecting…", { timeout: 10_000 });

  // In the game: one over the internet, one whose connection stayed on the Wi-Fi.
  phase = "playing";
  const ben = panel.getByTestId("pt-player-2");
  await expect(ben.getByTestId("pt-player-name")).toHaveText("Ben", { timeout: 10_000 });
  await expect(ben.getByTestId("pt-player-via")).toHaveText("Internet direct");
  await expect(ben.getByTestId("pt-player-delay")).toHaveText("35 ms");
  await expect(panel.getByTestId("pt-player-3").getByTestId("pt-player-via")).toHaveText("Home network");
  await expect(panel.getByTestId("pt-net-peer")).toHaveCount(0);
});

test("no internet here: the Wi-Fi's link, said to work on the Wi-Fi only, then the one link again", async ({ page }) => {
  await seedConsole(page);
  let phase: Phase = "offline";
  await fakeEngine(page, () => phase);
  const panel = await openPlayTogether(page);
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK, { timeout: 15_000 });
  await expect(panel.getByTestId("pt-wifi-only")).toHaveText("Works on your Wi-Fi only (no internet right now).");
  // The hub comes back: the engine says so on its next answer.
  phase = "internet";
  await expect(panel.getByTestId("pt-url")).toHaveText(NET_URL, { timeout: 10_000 });
  await expect(panel.getByTestId("pt-wifi-only")).toHaveCount(0);
});

test("Only on my Wi-Fi in the settings: the LAN link and the way to change it", async ({ page }) => {
  await seedConsole(page);
  const posts = await fakeEngine(page, () => "internet", "wifi");
  const panel = await openPlayTogether(page);
  await expect(panel.getByTestId("pt-url")).toHaveText(LINK);
  await expect(panel.getByTestId("pt-wifi-only")).toContainText("Settings › App › Play Together");
  await expect(panel.getByTestId("pt-addresses-internet-support")).toHaveCount(0);
  expect(posts.some((p) => p.path === "/api/join/invite" && (p.body.share || p.body.internet))).toBe(false);
});

test("Play together page: one link, nothing to choose; Start and invite from the keyboard", async ({ page }) => {
  await seedConsole(page);
  const posts = await fakeEngine(page, () => "internet");
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await page.getByRole("radio", { name: "Play Together" }).click({ timeout: 30_000 });
  const start = page.getByTestId("pt-start");
  await expect(start).toContainText("No app needed");
  await expect(start.getByTestId("pt-start-net-internet-hint")).toContainText("Works from anywhere");
  await expect(start.getByTestId("pt-start-net-internet-support")).toContainText("a donation keeps it going");
  await expect(start.getByTestId("pt-start-net-home")).toHaveCount(0);
  // Nothing opened before the button.
  expect(posts.find((p) => p.path === "/api/join/open")).toBeUndefined();

  await start.getByTestId("pt-start-invite").focus();
  await page.keyboard.press("Enter");
  const panel = page.getByTestId("pt-panel");
  await expect(panel.getByTestId("pt-url")).toHaveText(NET_URL, { timeout: 15_000 });
  await expect.poll(() => posts.map((p) => p.path)).toContain("/api/join/open");
  // The invite takes the focus, and the page stays on Play together.
  await expect(panel).toBeFocused();
  await expect(page.getByRole("radio", { name: "Play Together" })).toHaveAttribute("aria-checked", "true");
  await expect(page.getByTestId("pt-start")).toHaveCount(0);
});

test("Play together page: Only on my Wi-Fi is said before the stream; Support opens Thanks", async ({ page }) => {
  await seedConsole(page);
  await fakeEngine(page, () => "internet", "wifi");
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await page.getByRole("radio", { name: "Play Together" }).click({ timeout: 30_000 });
  await expect(page.getByTestId("pt-start-net-off")).toContainText("Invites work only on your Wi-Fi");
  await expect(page.getByTestId("pt-start-net-off").getByRole("button", { name: "Open settings" })).toBeVisible();
});

test("Play together page: Support opens Thanks", async ({ page }) => {
  await seedConsole(page);
  await fakeEngine(page, () => "internet");
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await page.getByRole("radio", { name: "Play Together" }).click({ timeout: 30_000 });
  await page.getByTestId("pt-start-net-internet-donate").click();
  await expect(page).toHaveURL(/\/thanks$/);
});
