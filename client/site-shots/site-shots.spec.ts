import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// The website's screenshots (site/assets/shots), taken from the real app against a made-up
// console: fictional games with drawn covers (make-art.py), a drawn game picture for the stream,
// a generic console name, user and address. No real console, account, game or file appears.
//
// Every language the app has gets its own set, from the app running in that language: English
// in out/, the others in out/<code>/. SHOT_LANGS=de,fr takes only those.
//
//   cd client && npx playwright test -c site-shots.config.ts && node site-shots/to-webp.mjs

const HOST = "192.168.1.50";
const OUT = process.env.SHOTS_DIR ?? "site-shots/out";
const LANGS = (process.env.SHOT_LANGS ?? "en,zh-CN,zh-TW,hi,es,ar,bn,pl,pt-BR,ru,ja,de,fr,ko,tr,vi,id,it,th,hu,fa").split(",");
const art = (name: string) => readFileSync(new URL(`./art/${name}.png`, import.meta.url));

const GAMES: Array<[string, string, string, number]> = [
  ["PPSA90001", "Neon Drift", "PS5", 48.2e9],
  ["PPSA90002", "Skyward Isles", "PS5", 61.7e9],
  ["PPSA90003", "Iron Harbor", "PS5", 33.9e9],
  ["PPSA90004", "Lumen Trail", "PS5", 12.4e9],
  ["PPSA90005", "Pocket Galaxy", "PS5", 8.1e9],
  ["PPSA90006", "Frostline", "PS5", 27.5e9],
  ["CUSA90007", "Ember Knights", "PS4", 19.3e9],
  ["CUSA90008", "Tidal Run", "PS4", 6.6e9],
];
const NOW = 1761800000;

function library() {
  const games: Record<string, unknown> = {};
  GAMES.forEach(([id, title, platform, size], i) => {
    const file = `${title.replace(/ /g, "")}.pkg`;
    games[id] = {
      game_id: id,
      title,
      platform,
      sources: ["/Volumes/Games"],
      locations: [
        {
          root: "/Volumes/Games",
          container: "",
          name: file,
          type: "pkg",
          path: file,
          absolute_path: `/Volumes/Games/${file}`,
          size_bytes: size,
          added_ts: NOW - i * 86400,
          pkg: { platform, title, title_id: id, version: "01.000", kind: "base" },
        },
      ],
      total_size_bytes: size,
      copies: 1,
      is_duplicate: false,
      added_ts: NOW - i * 86400,
      local_cover: `covers/${id}.png`,
      cover_stamp: `1-${NOW}`,
    };
  });
  const total = GAMES.reduce((a, g) => a + g[3], 0);
  return {
    roots: ["/Volumes/Games"],
    missing_roots: [],
    generated_at: "2026-10-30T10:00:00Z",
    summary: { total_games: GAMES.length, total_locations: GAMES.length, total_size_bytes: total, duplicates_count: 0, reclaimable_bytes: 0 },
    games,
  };
}

const READINESS = {
  fw_magic: 0x10000000,
  has_per_user: 1,
  foreground_uid: 0x10000000,
  account_uid: 0x10000000,
  account_via: "foreground",
  user_slot: 1,
  account_id_b64: "AAAAAAAAAAE=",
  account_id_raw: 1,
  account_type: "np",
  service_enabled: 0,
  user_enabled: 0,
  symbols_ok: 1,
  registry_err: 0,
};

// Play Together (#641): the host's invite and two made-up friends, one playing as Player 2 and
// one asking to join (the shapes of e2e/play-together.spec.ts and join.rs). With `internet`, the
// invite also has its internet link (11.6.0, playTogetherInternet.test.tsx's shapes) and the
// friends play over the internet: a made-up room and key, never a real one.
const INVITE = "00112233445566778899aabbccddeeff";
const COMPUTER = HOST.replace(/\.50$/, ".20");
const LINK = `http://${COMPUTER}:19113/join#i=${INVITE}&n=Alex`;
// The shapes join/rtcTransport.ts accepts: a 22-character room and a 32-byte fingerprint.
const NET_ROOM = "Demo0Room0Not0Real0000";
const NET_FP = `${"AB:".repeat(31)}CD`;
const NET_FRAGMENT = `room=${NET_ROOM}&k=${INVITE}&fp=${NET_FP}&hub=hub.example.com&n=Alex`;
const NET_LINK = `https://ps5upload.phantomptr.com/join/#${NET_FRAGMENT}`;

function joinState(path: string, internet: boolean) {
  const invite = {
    key: INVITE,
    code: "K7M2QX",
    code_display: "K7M-2QX",
    url: LINK,
    urls: [LINK],
    tailscale: null,
    expires_in_s: 1800,
    ...(internet
      ? { internet: { url: NET_LINK, room: NET_ROOM, hub: "hub.example.com", fp: NET_FP, state: "hosted", error: null, router: null, peers: [] } }
      : {}),
  };
  const settings = {
    block_system_buttons: true,
    max_players: 3,
    ...(internet ? { stream_seat: false, internet_invites: true, hub_url: "wss://hub.example.com/ws", stun: [], upnp: true } : {}),
  };
  const net = internet ? { via: "internet", path: "direct", ice_rtt_ms: 38 } : {};
  if (path === "/api/join/invite") return invite;
  if (path === "/api/join/decide" || path === "/api/join/kick") return { ok: true };
  if (path === "/api/join/settings") return { settings };
  return {
    open: true,
    console: HOST,
    playing: true,
    host_away_s: null,
    invite,
    players_in: 1,
    guests: [
      { id: 4, name: "Sam", player: 2, state: "playing", connected: true, reconnect_left_s: null, rtt_ms: internet ? 38 : 24, queue: 0, skipped: 0, since_s: 640, why: null, ...net },
    ],
    pending: [{ id: 5, name: "Mia", state: "asking" }],
    settings,
    max_guests: 3,
    ws: `ws://${COMPUTER}:19113/api/join/ws`,
    ...(internet ? { internet: { room: NET_ROOM, peers: [] } } : {}),
  };
}

// Tailscale (12.0.0): the console and this computer on a made-up tailnet. Addresses in
// Tailscale's 100.64.0.0/10 range and a made-up tailnet name, never a real node or account.
const TS_DOMAIN = "tail0000.ts.net.";
const tsPeer = (name: string, ip: string, path: string, ms: number) => ({
  name: `${name}.${TS_DOMAIN}`,
  ip,
  online: path !== "idle",
  path,
  direct: path === "direct" ? "203.0.113.7:41641" : "",
  derp_region: 1,
  latency_ms: ms,
  last_rx_ms: 0,
});
const TS_NODE = {
  ok: true,
  enabled: true,
  state: "running",
  login_url: "",
  control_url: "https://controlplane.tailscale.com",
  home_derp: 1,
  derp_connected: true,
  home_latency_ms: 12,
  key_expiry: 0,
  error: "",
};
const TS_CONSOLE = {
  ...TS_NODE,
  hostname: "living-room-ps5u",
  ip: "100.101.102.50",
  name: `living-room-ps5u.${TS_DOMAIN}`,
  signed_in: true,
  allow_mode: "listed",
  allow: "100.101.102.20,100.101.102.31",
  blocked_recent: [{ ip: "100.101.102.77", name: `old-tablet.${TS_DOMAIN}`, port: 9120, proto: "tcp", count: 2, first: NOW - 600, last: NOW - 300 }],
  udp_port: 41641,
  endpoints: [],
  warning: "",
  serving: { udp_flows: 2, tcp_conns: 3 },
  peers: [
    tsPeer("work-laptop-ps5u", "100.101.102.20", "direct", 18.4),
    tsPeer("phone-ps5u", "100.101.102.31", "relayed", -1),
    tsPeer("old-tablet", "100.101.102.77", "idle", -1),
  ],
};
const TS_APP = {
  ...TS_NODE,
  hostname: "work-laptop-ps5u",
  ip: "100.101.102.20",
  name: `work-laptop-ps5u.${TS_DOMAIN}`,
  signed_in: true,
  backend: "builtin",
  default_hostname: "work-laptop-ps5u",
  peers: [
    tsPeer("living-room-ps5u", "100.101.102.50", "direct", 18.4),
    tsPeer("phone-ps5u", "100.101.102.31", "relayed", -1),
  ],
  consoles: [{ host: HOST, ip: "100.101.102.50", name: "living-room-ps5u", via: "direct", latency_ms: 18.4, tested_at: (NOW - 86400) * 1000 }],
};

// 12.1.0 / 12.2.0: the Consoles page, a device asking, Add a PS5, Invite a device, and Share.
// Made-up devices, keys and links (the shapes of e2e/consoles-page.spec.ts and share-panel.spec.ts).
const OFFICE = HOST.replace(/\.50$/, ".51");
const CONSOLE_KEY = "ab".repeat(32);
const HB_ID = "HBRW00001";
const SHARE_LINK = "https://ps5upload.phantomptr.com/share/#room=Demo0Share0Not0Real00&fp=AB&hub=hub.example.com";
const meshDevice = (id: string, name: string, kind: string, os: string, ip: string, online: boolean, via: string, agoS: number) => ({
  id: id.repeat(32).slice(0, 64), name, nick: "", kind, os, app: "12.3.0", first: 1, last: Date.now() - agoS * 1000, ip, online, blocked: false, activity: [], via,
});
function meshState(asking: boolean) {
  return {
    supported: true,
    rev: asking ? 2 : 1,
    now: Date.now(),
    you: "me".padEnd(64, "0"),
    helper: "12.3.0",
    settings: { require_approval: true, prune_days: 90 },
    asks: asking ? [{ id: "cd".repeat(32), name: "Mia's phone", ip: "192.168.1.34", at: Date.now() - 4000 }] : [],
    devices: [
      { ...meshDevice("me", "MacBook", "desktop", "macOS", COMPUTER, true, "", 0), id: "me".padEnd(64, "0") },
      meshDevice("11", "Pixel 9", "android", "Android 16", "192.168.1.31", true, "approved", 60),
      meshDevice("22", "Work laptop", "desktop", "Windows", "100.101.102.20", false, "tailscale", 86400),
      meshDevice("33", "iPad", "ios", "iPadOS", "192.168.1.32", false, "paired", 3 * 86400),
    ],
    locks: [],
    events: [],
    ev: 1,
  };
}
function consolesAnswer(asking: boolean) {
  return (path: string) => {
    if (path === "/api/mesh/state") return meshState(asking);
    if (path === "/api/mesh/me") return { id: "me", name: "MacBook", kind: "desktop", os: "macOS", app: "12.3.0" };
    if (path === "/api/mesh/device") return { supported: true, ok: true, rev: 3 };
    if (path === "/api/tailnet")
      return { ...TS_APP, consoles: [{ host: HOST, ip: "100.101.102.50", name: "living-room-ps5u", via: "lan", latency_ms: 18.4, tested_at: Date.now() - 86400_000 }] };
    if (path === "/api/ava1/console") return { key: CONSOLE_KEY, name: "PS5" };
    if (path === "/api/discover")
      return {
        ok: true, scanned_ms: 2900, browsed_services: [],
        candidates: [
          { ip: OFFICE, hostname: "PS5-Office", all_ips: [OFFICE], services: [], loader_port_open: true, payload_port_open: true, confidence: 90, likely_ps5: true },
          { ip: "192.168.1.60", hostname: "PS5-Bedroom", all_ips: ["192.168.1.60"], services: [], loader_port_open: true, payload_port_open: false, confidence: 60, likely_ps5: true },
        ],
      };
    if (path === "/api/tailnet/consoles")
      return {
        backend: "builtin", can_list: true,
        consoles: [
          { name: "office", tailnet_name: "office-ps5u", tailnet_ip: "100.101.102.51", lan_host: OFFICE, id: CONSOLE_KEY, reachable: true, via: "builtin", online: true, paired: false, hosts: [], listed: null, error: null },
        ],
      };
    return undefined;
  };
}
function shareAnswer(state: { share: Record<string, unknown> }) {
  return (path: string, req: import("@playwright/test").Request) => {
    const m = req.method();
    if (path === "/api/mesh/me") return { id: "me", name: "Alex's MacBook", kind: "desktop", os: "macOS", app: "12.3.0" };
    if (path === "/api/ps5/apps/installed") return { titles: [{ title_id: HB_ID, title_name: "Pocket Galaxy", origin: "folder" }] };
    if (path === `/api/games/${HB_ID}`)
      return {
        title_id: HB_ID, title: "Pocket Galaxy", platform: "PS5", cover: null, copies: [],
        consoles: [{ host: HOST, read_at: NOW, installed: true, version: "01.000", registered_from: `/user/app/${HB_ID}`, dlc_missing: [] }],
      };
    if (path === "/api/share/check") return { items: [{ item: 0, ok: true }] };
    if (path === "/api/ps5/saves/list")
      return { saves: [{ title_id: HB_ID, user_id: 0x10000000, path: `/user/home/10000000/savedata_prospero/${HB_ID}`, size: 1_200_000, mtime: NOW, kind: "ps5" }] };
    if (path === "/api/ps5/screenshots/list")
      return { items: [1, 2, 3, 4].map((n) => ({ path: `/user/av_contents/photo/${HB_ID}/shot${n}.jxr`, size: 2_000_000, mtime: NOW - n })) };
    if (path === "/api/ps5/videos/list") return { items: [{ path: `/user/av_contents/video/${HB_ID}/clip.webm`, size: 30_000_000, mtime: NOW }] };
    if (path === "/api/ps5/cheats/get") return { mods: [{ name: "Infinite fuel", on: true }, { name: "Warp drive", on: true }, { name: "All ships", on: false }] };
    if (path === "/api/share" && m === "POST") return state.share;
    if (path === "/api/share") return { shares: [], receives: [], history: [] };
    if (path === "/api/share/s1") return state.share;
    if (path === "/api/share/receive" && m === "POST") return { id: "r1" };
    if (path === "/api/share/receive/r1")
      return {
        id: "r1", state: "offered", from: "Sam", title: "Pocket Galaxy", title_id: HB_ID,
        items: [
          { id: "i1", kind: "app", name: "Pocket Galaxy", title_id: HB_ID, format: "folder", size: 182_000_000, files: 40, refused: null },
          { id: "i2", kind: "save", name: "Pocket Galaxy save", title_id: HB_ID, format: null, size: 1_200_000, files: 1, refused: null },
          { id: "i3", kind: "media", name: "4 screenshots, 1 clip", title_id: HB_ID, format: null, size: 38_000_000, files: 5, refused: null },
          { id: "i4", kind: "note", name: "Note", title_id: null, format: null, size: 40, files: 1, refused: null },
        ],
        accepted: [], expires_in_s: 3600, bytes_done: 0, bytes_total: 0, error: null, path: null, dest: null, received: [],
      };
    return undefined;
  };
}
const shareMade = (requests: unknown[]) => ({
  id: "s1", state: "open", from: "Alex's MacBook", title: "Pocket Galaxy", title_id: HB_ID, one_use: false,
  expires_in_s: 86_340, url: SHARE_LINK, lan_url: null, works: "anywhere", items: [], requests,
});

/** The made-up console, as the engine would answer for it. */
async function fakeConsole(
  page: Page,
  opts: {
    paired: boolean;
    theme: "light" | "dark";
    lang: string;
    internet?: boolean;
    tailnet?: boolean;
    /** A second PS5 on the list (12.2.0's Consoles page). */
    two?: boolean;
    /** Answers first; undefined falls through to the rest. */
    extra?: (path: string, req: import("@playwright/test").Request) => unknown;
  },
) {
  await page.addInitScript(
    ([host, paired, theme, now, lang, two]) => {
      const ls = window.localStorage;
      ls.setItem("ps5upload.theme", theme);
      ls.setItem("ps5upload.lang", lang);
      ls.setItem("ps5upload.host", host);
      ls.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({
          active_id: "p1",
          profiles: [
            {
              id: "p1",
              name: "Living Room PS5",
              host,
              ...(paired
                ? { wake_regist_key: "00000000000000000000000000000001", wake_rp_key: "00000000000000000000000000000002" }
                : {}),
            },
            ...(two ? [{ id: "p2", name: "Office PS5", host: host.replace(/\.50$/, ".51") }] : []),
          ],
        }),
      );
      // The last game seen playing, for Home's Continue.
      ls.setItem(
        "ps5upload.play_time.v2",
        JSON.stringify({ byHost: { [host]: { PPSA90001: 5400 } }, lastSeenByHost: { [host]: { PPSA90001: now * 1000 } }, lastSampleMs: 0 }),
      );
      if (paired) ls.setItem("ps5upload.play_setup.v1", JSON.stringify({ rest: { [host]: true }, played: {}, homeHidden: false }));
    },
    [HOST, opts.paired, opts.theme, NOW, opts.lang, !!opts.two] as const,
  );
  await page.route("**/api/collection/games/*/cover*", (route) => {
    const id = new URL(route.request().url()).pathname.split("/")[4];
    return route.fulfill({ contentType: "image/png", body: art(id) });
  });
  await page.route("**/api/ps5/app-icon*", (route) => {
    const id = new URL(route.request().url()).searchParams.get("title_id") ?? "PPSA90001";
    if (id === HB_ID) return route.fulfill({ contentType: "image/png", body: art("PPSA90005") });
    return route.fulfill({ contentType: "image/png", body: art(GAMES.some((g) => g[0] === id) ? id : "PPSA90001") });
  });
  let tick = 0;
  await page.route(
    (url) => url.pathname.startsWith("/api/") && !url.pathname.includes("/cover") && !url.pathname.includes("app-icon"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      const json = (b: unknown) => route.fulfill({ contentType: "application/json", body: JSON.stringify(b) });
      if (opts.extra) {
        const got = await opts.extra(path, route.request());
        if (got !== undefined) return json(got);
      }
      if (path.startsWith("/api/join")) return json(joinState(path, !!opts.internet));
      if (opts.tailnet) {
        if (path === "/api/ps5/tailnet") return json(TS_CONSOLE);
        if (path === "/api/tailnet") return json(TS_APP);
        if (path === "/api/tailnet/via") return json({ via: "direct", latency_ms: 18.4 });
      }
      switch (path) {
        case "/api/version":
          return json({ version: "12.3.0", caps: {} });
        case "/api/ps5/status":
          return json({ ok: true, command_count: 1, instance_id: 1, ps5_kernel: "releases/10.00", ucred_elevated: true, version: "12.3.0" });
        case "/api/ps5/rp/session":
          return json({ host: null });
        // Awake, as the console's own discovery reply says (the power card).
        case "/api/ps5/power/ddp-status":
          return json({
            code: 200, status_text: "Ok", host_id: "000000000000", host_name: "Living Room PS5", host_type: "PS5",
            system_version: "10000000", running_app_name: "", running_app_titleid: "",
          });
        case "/api/ps5/remoteplay/readiness":
          return json(READINESS);
        case "/api/profile/info":
          return json({
            ok: true,
            uid: 0x10000000,
            uid_hex: "0x10000000",
            username: "Player One",
            users: [
              { uid: 0x10000000, uid_hex: "0x10000000", username: "Player One" },
              { uid: 0x10000001, uid_hex: "0x10000001", username: "Player Two" },
            ],
            slots: [{ slot: 1, name: "Player One", type: "np", flags: 4098, id: "0x0000000000000001", activated: true, offline_activated: true }],
          });
        case "/api/profile/np/state":
          return json({
            ok: true, slot: 1, uid: 0x10000000, backup: false, backup_slot: 0, online_id: "player_one",
            dob: "2000-01-01", auth_dat: true, config_dat: true, sub_account: 0, offline_adult: 1, age: 26, login_flag: 1,
          });
        case "/api/profile/avatar/current":
          return json({ data_url: null });
        case "/api/collection/library":
          return json(library());
        case "/api/collection/settings":
          return json({ roots: ["/Volumes/Games"], refresh_secs: null, sweep_sidecars: false, trash_available: true });
        case "/api/collection/scan":
          return json({ running: false, deep: false, found: 8, done: 8, started_ms: 0, finished_ms: 0, error: null, games: 8, locations: 8 });
        case "/api/collection/console":
          return json({
            games: GAMES.slice(0, 5).map(([id]) => ({ game_id: id, installed: true, installed_version: "01.000", dlc_missing: [], non_package_copy: false })),
          });
        case "/api/ps5/apps/installed":
          return json({
            titles: GAMES.slice(0, 5).map(([id, title]) => ({ title_id: id, title_name: title, origin: "pkg", image_backed: false, source: "", system: false })),
            registered_unavailable: false,
            incomplete: false,
          });
        case "/api/games/installed":
          return json(GAMES.slice(0, 5).map(([id, title]) => ({ title_id: id, title, hosts: [HOST] })));
        case "/api/ps5/saves/list":
          return json({
            saves: GAMES.map(([id, , platform], i) => ({
              title_id: id,
              user_id: 0x10000000,
              path: platform === "PS5" ? `/user/home/10000000/savedata_prospero/${id}` : `/user/home/10000000/savedata/${id}`,
              size: (i + 2) * 1_572_864,
              mtime: NOW - i * 7200,
              kind: platform === "PS5" ? "ps5" : "ps4",
            })),
          });
        case "/api/ps5/list-dir":
          return json({
            entries: [
              { name: "homebrew", kind: "dir", size: 0, mtime: NOW - 3600 },
              { name: "games", kind: "dir", size: 0, mtime: NOW - 7200 },
              { name: "screenshots", kind: "dir", size: 0, mtime: NOW - 9000 },
              { name: "notes.txt", kind: "file", size: 2048, mtime: NOW - 600 },
              { name: "config.ini", kind: "file", size: 512, mtime: NOW - 86400 },
            ],
            truncated: false,
          });
        case "/api/ps5/files/prefs":
          return json({ supported: true, favorites: [], recent: [], last: null });
        case "/api/ps5/volumes":
          return json({
            volumes: [
              { path: "/data", fs_type: "ufs", total_bytes: 825e9, free_bytes: 410e9, writable: true },
              { path: "/mnt/ext0", fs_type: "exfat", total_bytes: 2000e9, free_bytes: 1240e9, writable: true },
            ],
          });
        case "/api/ps5/hw/temps":
          tick++;
          return json({
            cpu_temp: 50 + (tick % 4), soc_temp: 56 + (tick % 3), m2_temp: 41, cpu_freq_mhz: 3500, soc_clock_mhz: 0,
            soc_power_mw: 0, cpu_usage_pct: 18 + (tick % 7), fan_duty_pct: 35, product_shape: 0, fan_pinned_c: 0,
          });
        case "/api/ps5/hw/power":
          return json({
            operating_time_sec: 1_800_000, operating_time_hours: 500, operating_time_minutes: 0, boot_count: 120,
            power_consumption_mw: 0, load_avg_1m: 0.4, load_avg_5m: 0.3, load_avg_15m: 0.2,
          });
        case "/api/ps5/health/scan":
          return json({
            addr: HOST,
            duration_ms: 640,
            checks: [
              { id: "payload", title: "Helper running", category: "connectivity", status: "pass", detail: "Connected", remedy: "" },
              { id: "clock", title: "Clock in sync", category: "system", status: "pass", detail: "Off by 0s", remedy: "" },
            ],
            summary: { pass: 2, warn: 0, fail: 0, skip: 0 },
          });
        case "/api/ps5/rp/pad-link": {
          const url = `http://${HOST.replace(/\.50$/, ".20")}:19113/pad#t=00000000000000000000000000000001`;
          return json({
            open: true,
            url,
            urls: [url],
            pad_ws: `ws://${HOST.replace(/\.50$/, ".20")}:19113/api/pad/ws`,
            pads: [
              { id: 1, name: "Phone", watch: false, control: true },
              { id: 2, name: "Tablet", watch: true, control: false },
            ],
            pending: [],
          });
        }
        default:
          return json({});
      }
    },
  );
}

/** The drawn game picture as the engine's stream messages: the H.264 header, then frames. */
async function streamOf(page: Page, size: { w: number; h: number }): Promise<{ header: Buffer; au: Buffer }> {
  const png = art("stream").toString("base64");
  const r = await page.evaluate(
    async ({ png, w, h }) => {
      const img = await createImageBitmap(await (await fetch(`data:image/png;base64,${png}`)).blob());
      const cv = new OffscreenCanvas(w, h);
      cv.getContext("2d")!.drawImage(img, 0, 0, w, h);
      const out: Uint8Array[] = [];
      const enc = new VideoEncoder({
        output: (c) => {
          const d = new Uint8Array(c.byteLength);
          c.copyTo(d);
          out.push(d);
        },
        error: (e) => {
          throw e;
        },
      });
      enc.configure({ codec: "avc1.640028", width: w, height: h, bitrate: 20_000_000, framerate: 30, avc: { format: "annexb" } });
      const f = new VideoFrame(cv, { timestamp: 0 });
      enc.encode(f, { keyFrame: true });
      f.close();
      await enc.flush();
      const s = out[0];
      let idr = 0;
      for (let i = 0; i + 3 < s.length; i++) {
        if (s[i] === 0 && s[i + 1] === 0 && s[i + 2] === 1 && (s[i + 3] & 0x1f) === 5) {
          idr = i > 0 && s[i - 1] === 0 ? i - 1 : i;
          break;
        }
      }
      return { header: Array.from(s.subarray(0, idr)), au: Array.from(s.subarray(idr)) };
    },
    { png, w: size.w, h: size.h },
  );
  return { header: Buffer.from(r.header), au: Buffer.from(r.au) };
}

const rec = (kind: number, payload: Buffer) => Buffer.concat([Buffer.from([kind]), payload]);

async function fakeStream(page: Page, frames: { header: Buffer; au: Buffer }) {
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    let timer: ReturnType<typeof setInterval> | null = null;
    ws.onClose(() => timer && clearInterval(timer));
    ws.onMessage((m) => {
      if (typeof m !== "string") return;
      const msg = JSON.parse(m) as Record<string, unknown>;
      if ("ping" in msg) ws.send(JSON.stringify({ pong: msg.ping }));
      // The console's keyboard and mouse, made as asked.
      if ("kbm" in msg) {
        const k = msg.kbm as { on: boolean; mouse?: boolean; keyboard?: boolean };
        ws.send(JSON.stringify({ kbm: { on: k.on, mouse: k.on && !!k.mouse, keyboard: k.on && !!k.keyboard, err: null, reason: null } }));
      }
      if (!("host" in msg)) return;
      ws.send(JSON.stringify({ stage: "first_frame" }));
      ws.send(rec(0, frames.header));
      for (let i = 0; i < 3; i++) ws.send(rec(1, frames.au));
      timer = setInterval(() => ws.send(rec(1, frames.au)), 100);
    });
  });
}

const MAC_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/** A console that can make a keyboard and a mouse (FW 13.60), in the given mode for every game. */
async function fakeKbm(page: Page, mode: "native" | "mixed" | "controller" | null) {
  const json = (body: unknown) => ({ contentType: "application/json", body: JSON.stringify(body) });
  await page.route("**/api/version", (route) => route.fulfill(json({ version: "11.0.4", caps: {} })));
  await page.route("**/api/ps5/status*", (route) =>
    route.fulfill(json({ ok: true, command_count: 1, instance_id: 1, ps5_kernel: "releases/13.60", ucred_elevated: true })),
  );
  await page.route("**/api/ps5/vinput/caps*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ supported: true, mouse: true, keyboard: true, open: false, backend: "ps5", companion: "running" }),
    }),
  );
  if (mode) {
    await page.addInitScript((mode) => {
      const modes = { home: mode, PPSA90001: mode };
      window.localStorage.setItem("ps5upload.rp_kbm.v1", JSON.stringify({ modes, hinted: true }));
    }, mode);
  }
}

for (const lang of LANGS) {
  const dir = lang === "en" ? OUT : `${OUT}/${lang}`;
  const shot = (page: Page, name: string, theme: string) =>
    page.screenshot({ path: `${dir}/${name}-${theme}.png`, animations: "disabled", caret: "hide" });

  for (const theme of ["light", "dark"] as const) {
    test.describe(`${lang} ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5 });

      test(`home, setup, upload, games, saves, workshop, connection (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        await expect(page.getByTestId("home-play-continue")).toBeVisible({ timeout: 30_000 });
        await page.waitForTimeout(1500);
        await shot(page, "home", theme);

        for (const [route, name] of [
          ["/send", "upload"],
          ["/games", "games"],
          ["/files", "files"],
          ["/workshop", "workshop"],
          ["/connection", "connection"],
        ] as const) {
          await page.getByRole("navigation").first().locator(`a[href="${route}"]`).first().click().catch(async () => {
            await page.goto(route, { waitUntil: "domcontentloaded" });
          });
          await page.waitForTimeout(2500);
          // The site leaves out the package-format notice on Upload & Install (a details box).
          if (name === "upload") {
            await page.evaluate(() => {
              for (const d of Array.from(document.querySelectorAll("details"))) {
                if (/FPKG/.test(d.textContent ?? "")) (d as HTMLElement).style.display = "none";
              }
            });
          }
          await shot(page, name, theme);
        }
      });

      test(`setup checklist (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: false, theme, lang });
        await page.goto("/play/setup", { waitUntil: "domcontentloaded" });
        await expect(page.locator('[data-step="signin"]')).toHaveAttribute("data-state", "ok", { timeout: 30_000 });
        await page.waitForTimeout(800);
        await shot(page, "setup", theme);
      });

      test(`play and phones (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        const frames = await streamOf(page, { w: 1920, h: 1080 });
        await fakeStream(page, frames);
        await page.getByRole("navigation").first().locator('a[href="/play"]').first().click();
        await page.getByTestId("rp-start").click();
        const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
        await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width), { timeout: 20_000 }).toBe(1920);
        await page.getByTestId("rp-toolbar").hover();
        await page.waitForTimeout(600);
        await shot(page, "play", theme);
        await page.getByTestId("rp-phones").click();
        await page.getByTestId("rp-pad-mode-watch").click().catch(() => {});
        await page.waitForTimeout(800);
        await shot(page, "phones", theme);
      });
    });

    // Play Together's panel is tall: a bigger window (the same picture size) shows all of it.
    // Invited over the internet (11.6.0): the Internet choice, its link and QR code, and a friend
    // playing from elsewhere.
    test.describe(`${lang} play together ${theme}`, () => {
      test.use({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1.2 });

      test(`play together over the internet (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang, internet: true });
        await page.addInitScript(() => window.localStorage.setItem("ps5upload.pt_network", "internet"));
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        const frames = await streamOf(page, { w: 1920, h: 1080 });
        await fakeStream(page, frames);
        await page.getByRole("navigation").first().locator('a[href="/play"]').first().click();
        await page.getByTestId("rp-start").click();
        const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
        await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width), { timeout: 20_000 }).toBe(1920);
        await page.getByTestId("rp-toolbar").hover();
        await page.getByTestId("rp-together").click();
        const panel = page.getByTestId("pt-panel");
        await expect(panel.getByTestId("pt-url")).toHaveText(NET_LINK, { timeout: 10_000 });
        await expect(panel.getByTestId("pt-player-2").getByTestId("pt-player-name")).toHaveText("Sam");
        await expect(panel.getByTestId("pt-player-2").getByTestId("pt-player-via")).toBeVisible();
        await expect(page.getByTestId("pt-ask")).toBeVisible({ timeout: 10_000 });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        // The whole panel in view, below the app's top bar (the picture is under it).
        await panel.evaluate((el) => {
          let box = el.parentElement;
          while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
          if (box) box.scrollTop += el.getBoundingClientRect().top - 84;
        });
        await page.waitForTimeout(400);
        await shot(page, "together-net", theme);
      });
    });

    // 12.0.0: Play's three tabs, on Play together before a stream: Internet chosen, with its
    // note and the donation line, and Start and invite.
    test.describe(`${lang} play together tab ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5 });

      test(`play together tab (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang, internet: true });
        await page.addInitScript(() => window.localStorage.setItem("ps5upload.pt_network", "internet"));
        await page.goto("/play", { waitUntil: "domcontentloaded" });
        await page.getByRole("radiogroup").getByRole("radio").nth(1).click();
        await expect(page.getByTestId("pt-start-net-internet-support")).toBeVisible({ timeout: 20_000 });
        await expect(page.getByTestId("pt-start-invite")).toBeEnabled();
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "together-tab", theme);
      });
    });

    // 12.0.0: Tailscale. The console's panel on the Connection screen, this device's own in
    // Settings, and the status bar's "via Tailscale" while the home address doesn't answer.
    test.describe(`${lang} tailscale ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5 });
      // A panel at the top of its page's own scroll, below the app's top bar.
      const toTop = (el: HTMLElement | SVGElement) => {
        let box = el.parentElement;
        while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
        if (box) box.scrollTop += el.getBoundingClientRect().top - 96;
      };

      test(`tailscale: connection and settings (${theme})`, async ({ page }) => {
        // Internet invites on, so Settings › Play Together above it shows its own options.
        await fakeConsole(page, { paired: true, theme, lang, tailnet: true, internet: true });
        // 12.1.0: the console's Tailscale lives in Settings › This PS5, with who may reach it.
        await page.goto("/settings?tab=ps5&console=p1", { waitUntil: "domcontentloaded" });
        const panel = page.getByTestId("tailnet-panel");
        await expect(panel.getByTestId("tailnet-allow")).toBeVisible({ timeout: 30_000 });
        await expect(page.getByTestId("tailnet-chip")).toBeVisible({ timeout: 15_000 });
        await panel.evaluate(toTop);
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "tailscale-connection", theme);

        await page.goto("/settings", { waitUntil: "domcontentloaded" });
        const settings = page.getByTestId("tailscale-settings");
        await expect(settings.getByTestId("app-tailnet-peers")).toBeVisible({ timeout: 30_000 });
        await expect(page.getByTestId("tailnet-chip")).toBeVisible({ timeout: 15_000 });
        // The whole section, its heading too, near the bottom of the window above the status bar.
        await settings.evaluate((el) => {
          const sec = el.closest("section") ?? el;
          let box = sec.parentElement;
          while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
          if (box) box.scrollTop += sec.getBoundingClientRect().bottom - (window.innerHeight - 64);
        });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "tailscale-settings", theme);
      });
    });

    // 12.2.0: every PS5 on one page, a device asking to use one, Add a PS5 and Invite a device.
    test.describe(`${lang} consoles ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5 });
      const ready = async (page: Page) => {
        const card = page.locator("[data-scroll-root]:visible").getByTestId("console-card").first();
        await expect(card).toHaveAttribute("data-ready", "1", { timeout: 30_000 });
        return card;
      };

      test(`consoles, add, invite (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang, tailnet: true, two: true, extra: consolesAnswer(false) });
        await page.goto("/connection", { waitUntil: "domcontentloaded" });
        await ready(page);
        await page.mouse.move(8, 8);
        await page.waitForTimeout(1500);
        await shot(page, "consoles", theme);

        const main = page.locator("[data-scroll-root]:visible");
        await main.getByTestId("consoles-add-open").click();
        const add = page.getByRole("dialog").first();
        await expect(add.getByTestId("consoles-found-row").nth(1)).toBeVisible({ timeout: 30_000 });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(2500);
        await shot(page, "add-ps5", theme);
        await page.keyboard.press("Escape");
        await expect(add).toHaveCount(0);

        await main.getByTestId("consoles-invite-open").first().click();
        const invite = page.getByRole("dialog").first();
        await expect(invite.getByTestId("consoles-invite-qr")).toBeVisible({ timeout: 15_000 });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "invite", theme);
      });

      test(`a device asking, and who may use it (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang, tailnet: true, extra: consolesAnswer(true) });
        await page.goto("/connection", { waitUntil: "domcontentloaded" });
        const card = await ready(page);
        // 12.3: a PS5 on Tailscale that isn't on the list, offered at the top of the page.
        await expect(page.locator("[data-scroll-root]:visible").getByTestId("consoles-ts-found")).toBeVisible({ timeout: 30_000 });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "ts-found", theme);
        await expect(card.getByTestId("consoles-ask")).toBeVisible({ timeout: 15_000 });
        await card.getByTestId("consoles-devices-toggle").click();
        await expect(card.getByTestId("consoles-trusted-device")).toHaveCount(4);
        await card.getByTestId("consoles-ask").evaluate((el) => {
          let box = el.parentElement;
          while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
          if (box) box.scrollTop += el.getBoundingClientRect().top - 110;
        });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "approval", theme);
      });
    });

    // 12.1.0: Share. A homebrew app with its save, screenshots and cheats, then the link with a
    // friend asking; and a friend's Receive.
    test.describe(`${lang} share ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 860 }, deviceScaleFactor: 1.5 });

      test(`share and receive (${theme})`, async ({ page }) => {
        const state = { share: shareMade([]) as Record<string, unknown> };
        await page.addInitScript((id) => window.localStorage.setItem("ps5upload.rp_controls.v1", JSON.stringify({ [id]: {} })), HB_ID);
        await fakeConsole(page, { paired: true, theme, lang, extra: shareAnswer(state) });
        await page.goto(`/games/${HB_ID}`, { waitUntil: "domcontentloaded" });
        await page.getByTestId("game-share").click({ timeout: 60_000 });
        await expect(page.getByTestId("share-panel")).toBeVisible();
        await expect(page.getByTestId("share-app")).toHaveAttribute("data-state", "on", { timeout: 15_000 });
        await page.getByTestId("share-save").getByRole("checkbox").click();
        await page.getByTestId("share-media").getByRole("checkbox").click();
        await page.getByTestId("share-note").fill("Try the asteroid level!");
        await page.getByTestId("share-panel").evaluate((el) => el.parentElement?.scrollTo(0, 0));
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "share", theme);

        state.share = shareMade([
          { request: 1, name: "Sam", wants: ["i1", "i2"], wants_names: ["Pocket Galaxy", "Pocket Galaxy save"], state: "asking", bytes_done: 0, bytes_total: 0, why: null },
          { request: 2, name: "Jo", wants: ["i1"], wants_names: ["Pocket Galaxy"], state: "sending", bytes_done: 61_000_000, bytes_total: 182_000_000, why: null },
        ]);
        await page.getByTestId("share-go").click();
        await expect(page.getByTestId("share-qr")).toBeVisible();
        await expect(page.getByTestId("share-request")).toHaveCount(2, { timeout: 10_000 });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "share-link", theme);

        await page.goto(`/share?receive=${encodeURIComponent(SHARE_LINK)}`, { waitUntil: "domcontentloaded" });
        await expect(page.getByTestId("receive-link")).toHaveValue(SHARE_LINK, { timeout: 60_000 });
        await page.getByTestId("receive-open").click();
        await expect(page.getByTestId("receive-offer")).toBeVisible();
        await expect(page.getByTestId("receive-item")).toHaveCount(4);
        await page.mouse.move(8, 8);
        await page.waitForTimeout(800);
        await shot(page, "receive", theme);
      });
    });

    // The chip close up: a sharper picture, cut to the chip and what sits beside it.
    test.describe(`${lang} tailscale chip ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 3 });

      test(`tailscale: the status bar's chip (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang, tailnet: true });
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        const chip = page.getByTestId("tailnet-chip");
        await expect(chip).toBeVisible({ timeout: 30_000 });
        await page.mouse.move(640, 300);
        await page.waitForTimeout(800);
        const b = (await chip.boundingBox())!;
        const padX = 14;
        const padY = 8;
        await page.screenshot({
          path: `${dir}/tailscale-chip-${theme}.png`,
          animations: "disabled",
          caret: "hide",
          clip: { x: b.x - padX, y: b.y - padY, width: b.width + padX * 2, height: b.height + padY * 2 },
        });
      });
    });

    test.describe(`${lang} keyboard & mouse ${theme}`, () => {
      test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5, userAgent: MAC_UA });

      test(`kbm: the first capture and the release chip (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await fakeKbm(page, "native");
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        const frames = await streamOf(page, { w: 1920, h: 1080 });
        await fakeStream(page, frames);
        await page.getByRole("navigation").first().locator('a[href="/play"]').first().click();
        await page.getByTestId("rp-start").click();
        const stage = page.getByTestId("rp-stage").filter({ visible: true });
        const canvas = stage.getByTestId("rp-canvas");
        await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width), { timeout: 20_000 }).toBe(1920);
        // Captured with a click on the picture: the first card, with the bar showing the mode.
        const box = (await stage.boundingBox())!;
        await page.mouse.click(box.x + box.width / 2, box.y + box.height - 60);
        await expect(page.getByTestId("rp-kbm-card")).toBeVisible();
        await page.getByTestId("rp-toolbar").hover({ position: { x: 12, y: 12 } });
        await page.waitForTimeout(500);
        await shot(page, "kbm", theme);
        // The card goes by itself; the faint chip stays while captured.
        await expect(page.getByTestId("rp-kbm-card")).toHaveCount(0, { timeout: 10_000 });
        await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
        await expect(page.getByTestId("rp-kbm-chip")).toBeVisible();
        // Only for social posts (SOCIAL_DIR=…): the site has no place for it.
        if (process.env.SOCIAL_DIR && lang === "en") {
          await page.waitForTimeout(4000); // the bar hides
          await page.screenshot({ path: `${process.env.SOCIAL_DIR}/kbm-chip-${theme}.png`, animations: "disabled", caret: "hide" });
        }
      });

      test(`kbm settings (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await fakeKbm(page, "native");
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        const frames = await streamOf(page, { w: 1920, h: 1080 });
        await fakeStream(page, frames);
        await page.getByRole("navigation").first().locator('a[href="/play"]').first().click();
        await page.getByTestId("rp-start").click();
        const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
        await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width), { timeout: 20_000 }).toBe(1920);
        await page.getByTestId("rp-toolbar").hover();
        await page.getByTestId("rp-settings-button").click();
        await page.getByTestId("rp-settings-tab-controls").click();
        await expect(page.getByTestId("rp-kbm").getByTestId("rp-kbm-mode-native")).toHaveAttribute("aria-checked", "true");
        await page.getByTestId("rp-sc-release").getByTestId("rp-sc-release-change").click();
        await expect(page.getByTestId("rp-sc-release-listening")).toBeVisible();
        // The mode section at the top of Settings' own scroll (the page itself stays put).
        await page.getByTestId("rp-kbm").evaluate((el) => {
          let box = el.parentElement;
          while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
          if (box) box.scrollTop += el.getBoundingClientRect().top - box.getBoundingClientRect().top;
          document.scrollingElement?.scrollTo(0, 0);
          document.querySelectorAll("main").forEach((m) => m.scrollTo(0, 0));
        });
        await page.mouse.move(8, 8);
        await page.waitForTimeout(600);
        await shot(page, "kbm-settings", theme);
      });
    });

    test.describe(`${lang} phone sideways ${theme}`, () => {
      test.use({ viewport: { width: 844, height: 390 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });

      test(`phone touchpad (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await fakeKbm(page, null);
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        const frames = await streamOf(page, { w: 1280, h: 720 });
        await fakeStream(page, frames);
        await page.goto("/play", { waitUntil: "domcontentloaded" });
        await page.getByTestId("rp-start").tap();
        const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
        await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width), { timeout: 20_000 }).toBe(1280);
        await page.getByTestId("rp-touch-mode").getByTestId("rp-touch-mode-touchpad").tap();
        await expect(page.getByTestId("rp-touchpad")).toBeVisible();
        await page.waitForTimeout(800);
        await shot(page, "m-touchpad", theme);
      });

      test(`a friend's phone (${theme})`, async ({ page }) => {
        const html = readFileSync(new URL("../../engine/crates/ps5upload-engine/src/join_page.html", import.meta.url), "utf8");
        await page.route("**/join", (route) => route.fulfill({ contentType: "text/html", body: html }));
        await page.addInitScript(
          ([lang, theme]) => {
            const ls = window.localStorage;
            ls.setItem("ps5upload.lang", lang);
            ls.setItem("ps5upload.theme", theme);
            ls.setItem("ps5upload.openIn", "browser");
            ls.setItem("ps5upload.join.guided", "1");
          },
          [lang, theme] as const,
        );
        await page.goto("/", { waitUntil: "domcontentloaded" });
        const frames = await streamOf(page, { w: 1280, h: 720 });
        await page.routeWebSocket(/\/api\/join\/ws/, (ws) => {
          let timer: ReturnType<typeof setInterval> | null = null;
          ws.onClose(() => timer && clearInterval(timer));
          ws.onMessage((m) => {
            if (typeof m !== "string") return;
            const v = JSON.parse(m) as { hello?: unknown; ping?: number };
            if (v.ping !== undefined) ws.send(JSON.stringify({ pong: v.ping }));
            if (!v.hello) return;
            ws.send(JSON.stringify({ stage: "joined", player: 2, token: "T2", hevc: false }));
            ws.send(JSON.stringify({ status: { player: 2, state: "playing", rtt_ms: 24, host: "Alex", block_system: true } }));
            ws.send(rec(0, frames.header));
            for (let i = 0; i < 3; i++) ws.send(rec(1, frames.au));
            timer = setInterval(() => ws.send(rec(1, frames.au)), 100);
          });
        });
        await page.goto(`/join#i=${INVITE}&n=Alex`, { waitUntil: "domcontentloaded" });
        await page.getByTestId("join-name").fill("Sam");
        await page.getByTestId("join-go").tap();
        await expect(page.getByTestId("join-player")).toBeAttached({ timeout: 30_000 });
        await expect.poll(() => page.getByTestId("watch-canvas").evaluate((c) => (c as HTMLCanvasElement).width), { timeout: 20_000 }).toBe(1280);
        // The buttons start hidden on touch; the first-time hint goes after 6 s.
        await expect(page.getByTestId("join-bar")).toHaveAttribute("data-shown", "0", { timeout: 10_000 });
        await expect(page.getByTestId("join-hold-hint")).toHaveCount(0, { timeout: 10_000 });
        await page.waitForTimeout(400);
        await shot(page, "m-guest", theme);
      });
    });

    test.describe(`${lang} phone ${theme}`, () => {
      test.use({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });

      test(`phone screens (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        await expect(page.getByTestId("home-play")).toBeVisible({ timeout: 30_000 });
        await page.waitForTimeout(1500);
        await shot(page, "m-home", theme);
        await page.goto("/games", { waitUntil: "domcontentloaded" });
        await page.waitForTimeout(2500);
        // Down to the covers.
        await page.evaluate(() => {
          const img = document.querySelector('main img[src*="/cover"]');
          img?.scrollIntoView({ block: "start" });
          document.querySelector("main")?.scrollBy?.(0, -90);
          window.scrollBy(0, -90);
        });
        await page.waitForTimeout(600);
        await shot(page, "m-games", theme);
        const frames = await streamOf(page, { w: 1280, h: 720 });
        await fakeStream(page, frames);
        await page.goto("/play", { waitUntil: "domcontentloaded" });
        await page.getByTestId("rp-start").tap();
        const canvas = page.getByTestId("rp-stage").filter({ visible: true }).getByTestId("rp-canvas");
        await expect.poll(() => canvas.evaluate((c: HTMLCanvasElement) => c.width), { timeout: 20_000 }).toBe(1280);
        await page.waitForTimeout(800);
        await shot(page, "m-play", theme);
      });

      // The website's guest page (site/join/, built by vite.join.config.ts), opened from an
      // internet invite on a phone: the name, then Join. It is dark either way, so one picture.
      if (theme === "light") {
        test(`a friend's internet invite, in a phone browser`, async ({ page }) => {
          const html = readFileSync(new URL("../../site/join/index.html", import.meta.url), "utf8");
          await page.route("**/join/", (route) => route.fulfill({ contentType: "text/html", body: html }));
          await page.addInitScript(
            ([lang]) => {
              window.localStorage.setItem("ps5upload.lang", lang);
              window.localStorage.setItem("ps5upload.join.name", "Sam");
            },
            [lang] as const,
          );
          await page.goto(`/join/#${NET_FRAGMENT}`, { waitUntil: "domcontentloaded" });
          // 12.3: an invite offers the app first; the picture is the in-browser name step.
          await expect(page.getByTestId("open-in-gate")).toBeVisible({ timeout: 30_000 });
          await page.getByTestId("open-in-browser").first().click();
          await expect(page.getByTestId("join-name-step")).toBeVisible({ timeout: 30_000 });
          await page.getByTestId("join-name").fill("Sam");
          await page.getByTestId("join-name").blur();
          await page.waitForTimeout(400);
          await page.screenshot({ path: `${dir}/m-net-join.png`, animations: "disabled", caret: "hide" });
        });
      }
    });
  }
}
