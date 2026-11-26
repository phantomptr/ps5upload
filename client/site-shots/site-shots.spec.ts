import { readFileSync } from "node:fs";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { shotsVersion } from "./shotsVersion";

// The website's screenshots (site/assets/shots), taken from the real app against a made-up
// console: fictional games with drawn covers (make-art.py), a drawn game picture for the stream,
// a generic console name, user and address. No real console, account, game or file appears.
//
// What may be in a picture, checked on every change here:
//   - games: invented titles only, with ids in the PPSA9xxxx / CUSA9xxxx range no real game uses;
//   - people: first names and "Player One", no account names, emails or online ids;
//   - addresses: 192.168.1.x at home, 100.101.102.x on the made-up tailnet, example.com hosts;
//   - files: the owner's own backups under /Volumes/Games and on the console's drives, named
//     after the game ("Neon Drift.pkg"), never after a download or a release;
//   - no serial numbers, hardware addresses, keys or tokens (the invite keys here are counting
//     patterns), and nothing from the computer that takes the pictures.
//
// Every language the app has gets its own set, from the app running in that language: English
// in out/, the others in out/<code>/. SHOT_LANGS=de,fr takes only those; SHOTS_PORT=14620 uses
// a dev server of its own instead of one already on 1420.
// The sidebar shows the version of the release the pictures are for: the repo's VERSION plus one
// patch level, or SHOTS_VERSION=12.7.0 for another.
//
//   cd client && npx playwright test -c site-shots.config.ts && node site-shots/to-webp.mjs

const HOST = "192.168.1.50";
const OUT = process.env.SHOTS_DIR ?? "site-shots/out";
const LANGS = (process.env.SHOT_LANGS ?? "en,zh-CN,zh-TW,hi,es,ar,bn,pl,pt-BR,ru,ja,de,fr,ko,tr,vi,id,it,th,hu,fa").split(",");
const art = (name: string) => readFileSync(new URL(`./art/${name}.png`, import.meta.url));
/** The version the pictures show (shotsVersion.ts): SHOTS_VERSION, else the repo's VERSION plus one
 *  patch level. They are taken before the release bumps it, and showed the version before. The
 *  sidebar reads it from the engine's /api/version, which is stubbed with it below. */
const VERSION = shotsVersion(process.env.SHOTS_VERSION, readFileSync(new URL("../../VERSION", import.meta.url), "utf8"));
const FIRMWARE = "10.01";

const ROOT = "/Volumes/Games";
type CopyType = "pkg" | "folder" | "mount.exfat" | "mount.ffpfsc";
/** The made-up library: id, title, platform, size, version, and how the copy is kept. */
const GAMES: Array<[string, string, "PS5" | "PS4", number, string, CopyType]> = [
  ["PPSA90001", "Neon Drift", "PS5", 48.2e9, "01.000", "pkg"],
  ["PPSA90002", "Skyward Isles", "PS5", 61.7e9, "01.000", "pkg"],
  ["PPSA90003", "Iron Harbor", "PS5", 33.9e9, "01.002", "mount.exfat"],
  ["PPSA90004", "Lumen Trail", "PS5", 12.4e9, "01.000", "pkg"],
  ["PPSA90005", "Pocket Galaxy", "PS5", 8.1e9, "01.000", "folder"],
  ["PPSA90006", "Frostline", "PS5", 27.5e9, "01.000", "pkg"],
  ["CUSA90007", "Ember Knights", "PS4", 19.3e9, "01.00", "pkg"],
  ["CUSA90008", "Tidal Run", "PS4", 6.6e9, "01.00", "pkg"],
  ["PPSA90009", "Clockwork Garden", "PS5", 15.8e9, "01.001", "mount.ffpfsc"],
  ["PPSA90010", "Lantern Marsh", "PS5", 22.6e9, "01.000", "pkg"],
  ["PPSA90011", "Copper Canyon", "PS5", 41.3e9, "01.003", "pkg"],
  ["PPSA90012", "Kite Season", "PS5", 3.4e9, "01.000", "mount.exfat"],
  ["CUSA90013", "Mosswood Tales", "PS4", 9.8e9, "01.00", "pkg"],
  ["CUSA90014", "Midnight Tram", "PS4", 4.2e9, "01.00", "pkg"],
];
/** Updates and add-ons kept beside their game: kind, what the file is called, version, size. */
const EXTRAS: Record<string, Array<["patch" | "dlc", string, string, number]>> = {
  PPSA90001: [["patch", "update 01.004", "01.004", 3.1e9]],
  PPSA90002: [["dlc", "Storm Season", "01.000", 4.4e9]],
  PPSA90006: [["patch", "update 01.002", "01.002", 1.7e9]],
  CUSA90007: [
    ["patch", "update 01.32", "01.32", 2.3e9],
    ["dlc", "The Ashen Keep", "01.00", 1.2e9],
  ],
};
/** What the console has installed, and at which version. */
const INSTALLED: Record<string, string> = {
  PPSA90001: "01.000",
  PPSA90002: "01.000",
  PPSA90003: "01.002",
  PPSA90005: "01.000",
  CUSA90007: "01.32",
  PPSA90009: "01.001",
  PPSA90012: "01.000",
  CUSA90013: "01.00",
};
// 21 November 2026, early evening.
const NOW = Math.floor(Date.UTC(2026, 10, 21, 18, 40) / 1000);
const EXT: Record<CopyType, string> = { pkg: ".pkg", folder: "", "mount.exfat": ".exfat", "mount.ffpfsc": ".ffpfsc" };
const contentId = (id: string, title: string, tail = "") =>
  `UP9000-${id}_00-${(title.replace(/[^A-Za-z]/g, "").toUpperCase() + tail).padEnd(16, "0").slice(0, 16)}`;
const extraName = (title: string, kind: string, label: string) => (kind === "patch" ? `${title} (${label}).pkg` : `${title} - ${label}.pkg`);

function library() {
  const games: Record<string, unknown> = {};
  let locations = 0;
  let total = 0;
  GAMES.forEach(([id, title, platform, size, version, type], i) => {
    const container = `${platform}/${title}`;
    const added = NOW - i * 5 * 86400 - i * 4321;
    const copy = (name: string, type: string, bytes: number, pkg: Record<string, unknown>) => ({
      root: ROOT,
      container,
      name,
      type,
      path: `${container}/${name}`,
      absolute_path: `${ROOT}/${container}/${name}`,
      size_bytes: bytes,
      added_ts: added,
      pkg: { platform, title, title_id: id, ...pkg },
    });
    const copies = [copy(`${title}${EXT[type]}`, type, size, { content_id: contentId(id, title), version, kind: "base" })];
    for (const [kind, label, v, bytes] of EXTRAS[id] ?? []) {
      copies.push(copy(extraName(title, kind, label), "pkg", bytes, { content_id: contentId(id, title, kind === "dlc" ? "DLC1" : ""), version: v, kind }));
    }
    const bytes = copies.reduce((a, c) => a + c.size_bytes, 0);
    locations += copies.length;
    total += bytes;
    games[id] = {
      game_id: id,
      title,
      platform,
      sources: [ROOT],
      locations: copies,
      total_size_bytes: bytes,
      copies: 1,
      is_duplicate: false,
      added_ts: added,
      local_cover: `covers/${id}.png`,
      cover_stamp: `1-${NOW}`,
    };
  });
  return {
    roots: [ROOT],
    missing_roots: [],
    generated_at: new Date(NOW * 1000).toISOString(),
    summary: { total_games: GAMES.length, total_locations: locations, total_size_bytes: total, duplicates_count: 0, reclaimable_bytes: 0 },
    games,
  };
}

/** Each game against the console: installed or not, and the update or add-on it could take. */
function consoleGames() {
  return GAMES.map(([id, title, platform]) => {
    const offers = (EXTRAS[id] ?? []).map(([kind, label, v, bytes]) => ({
      kind,
      offer: {
        path: `${ROOT}/${platform}/${title}/${extraName(title, kind, label)}`,
        name: extraName(title, kind, label),
        version: v,
        content_id: contentId(id, title, kind === "dlc" ? "DLC1" : ""),
        title: kind === "patch" ? title : `${title}: ${label}`,
        size_bytes: bytes,
        category: kind === "patch" ? "gp" : "ac",
      },
    }));
    const have = INSTALLED[id];
    const update = offers.find((o) => o.kind === "patch" && have && o.offer.version > have)?.offer;
    return {
      game_id: id,
      installed: !!have,
      ...(have ? { installed_version: have } : {}),
      ...(update ? { update } : {}),
      dlc_missing: have && id === "PPSA90002" ? offers.filter((o) => o.kind === "dlc").map((o) => o.offer) : [],
      non_package_copy: false,
    };
  });
}

/** The console's drives and folders, as Files shows them: the owner's games, images and backups. */
type Entry = { name: string; kind: "dir" | "file"; size: number; mtime: number };
const dirEntry = (name: string, agoH: number): Entry => ({ name, kind: "dir", size: 0, mtime: NOW - agoH * 3600 - 947 });
const fileEntry = (name: string, size: number, agoH: number): Entry => ({ name, kind: "file", size, mtime: NOW - agoH * 3600 - 1371 });
const FILES_START = "/data/games";
const LISTINGS: Record<string, Entry[]> = {
  "/data": [
    dirEntry("games", 3),
    dirEntry("homebrew", 26),
    dirEntry("payloads", 190),
    dirEntry("save-backups", 20),
    dirEntry("screenshots", 49),
    fileEntry("controller-layout.json", 3_184, 310),
    fileEntry("fan-curve.json", 742, 96),
    fileEntry("notes.txt", 2_048, 5),
  ],
  [FILES_START]: [
    dirEntry("Copper Canyon", 3),
    dirEntry("Pocket Galaxy", 172),
    dirEntry("Skyward Isles", 51),
    fileEntry("Clockwork Garden.ffpfsc", 11.9e9, 220),
    fileEntry("Iron Harbor.exfat", 33.9e9, 75),
    fileEntry("Kite Season.exfat", 3.4e9, 29),
    fileEntry("Lantern Marsh.pkg", 22.6e9, 8),
    fileEntry("Midnight Tram.pkg", 4.2e9, 340),
    fileEntry("Mosswood Tales save notes.txt", 1_612, 12),
  ],
  "/mnt/ext0": [dirEntry("games", 100), dirEntry("captures", 60), dirEntry("save-backups", 20)],
};
const VOLUMES = [
  { path: "/data", fs_type: "ufs", total_bytes: 824.6e9, free_bytes: 268.3e9, writable: true, is_placeholder: false },
  { path: "/mnt/ext0", fs_type: "exfat", total_bytes: 2000.4e9, free_bytes: 1213.7e9, writable: true, is_placeholder: false },
];

/** An upload in progress, as the app's own task list holds it: the status bar names it with its
 *  speed and time left (the module the app itself loaded, so this is the app's own store). */
async function runningUpload(page: Page) {
  await page.evaluate(async (host) => {
    const url = "/src/state/tasks.ts";
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const mod: any = await import(/* @vite-ignore */ url);
    const s = mod.useTaskStore.getState();
    const id = s.registerTask({ kind: "upload-dir", origin: "site-shots", label: "Copper Canyon", consoleId: host });
    s.updateTask(id, {
      stage: "Sending",
      progress: { current: 15.42e9, total: 41.3e9, unit: "bytes" },
      rate: { bytesPerSec: 109.4e6 },
      eta: 235,
    });
  }, HOST);
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
  id: id.repeat(32).slice(0, 64), name, nick: "", kind, os, app: VERSION, first: 1, last: Date.now() - agoS * 1000, ip, online, blocked: false, activity: [], via,
});
function meshState(asking: boolean) {
  return {
    supported: true,
    rev: asking ? 2 : 1,
    now: Date.now(),
    you: "me".padEnd(64, "0"),
    helper: VERSION,
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
    if (path === "/api/mesh/me") return { id: "me", name: "MacBook", kind: "desktop", os: "macOS", app: VERSION };
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
    if (path === "/api/mesh/me") return { id: "me", name: "Alex's MacBook", kind: "desktop", os: "macOS", app: VERSION };
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

/** Scrolls the element's own scrolling box so the element's top sits `y` px down the window. */
const scrollToY = (el: HTMLElement | SVGElement, y: number) => {
  let box = el.parentElement;
  while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
  if (box) box.scrollTop += el.getBoundingClientRect().top - y;
};

/** The first covers have loaded: no grey boxes in the picture. */
const coversLoaded = (page: Page) =>
  page.waitForFunction(
    () => {
      const imgs = Array.from(document.querySelectorAll<HTMLImageElement>('main img[src*="/cover"]')).filter((i) => i.getBoundingClientRect().top < window.innerHeight);
      return imgs.length > 0 && imgs.every((i) => i.complete && i.naturalWidth > 0);
    },
    undefined,
    { timeout: 90_000 },
  );

/** A picture of some elements together, with a little of the page around them. */
async function shotAround(page: Page, parts: Locator[], path: string, pad = { x: 10, y: 10 }) {
  const boxes = (await Promise.all(parts.map((l) => l.boundingBox()))).filter((b) => !!b);
  const x0 = Math.min(...boxes.map((b) => b.x)) - pad.x;
  const y0 = Math.min(...boxes.map((b) => b.y)) - pad.y;
  const x1 = Math.max(...boxes.map((b) => b.x + b.width)) + pad.x;
  const y1 = Math.max(...boxes.map((b) => b.y + b.height)) + pad.y;
  await page.screenshot({ path, animations: "disabled", caret: "hide", clip: { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } });
}

/** A finger held on `target`, as a phone sends it (e2e/files-phone-header.spec.ts). */
async function longPress(page: Page, target: Locator) {
  await target.scrollIntoViewIfNeeded();
  const box = (await target.boundingBox())!;
  const x = box.x + Math.min(40, box.width / 2);
  const y = box.y + box.height / 2;
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
  await page.waitForTimeout(800);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await cdp.detach();
}

/** The game Convert checks in the pictures: a folder kept in zstd asset packs. */
const CONVERT_SOURCE = `${ROOT}/PS5/Pocket Galaxy`;
const RENAMED = "Lantern Marsh";

/** Upload & Install's queue, mid-run: one game copied, one on its way, two packages waiting.
 *  Set in the app's own queue store, so the rows are the app's rows. */
async function uploadQueue(page: Page) {
  await page.evaluate(
    async ({ host, now, root }) => {
      const url = "/src/state/uploadQueue.ts";
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const mod: any = await import(/* @vite-ignore */ url);
      const row = (name: string, kind: string, from: string, patch: Record<string, unknown>) => ({
        id: `q-${name}`, sourceKind: kind, sourcePath: `${root}/${from}`, displayName: name,
        resolvedDest: `/data/games/${name}`, addr: host, strategy: "resume", reconcileMode: "fast",
        excludes: [], mountAfterUpload: false, mountReadOnly: false, registerAfterUpload: false,
        txIdHex: `site${name.length}`, status: "pending", bytesSent: 0, totalBytes: 0,
        bytesPerSec: 0, filesFinalized: 0, filesFinalizingTotal: 0, mountedAt: null,
        registeredAs: null, mountWarnings: [], error: null, errorReason: null, errorDetail: null,
        addedAt: now - 1_500_000, startedAt: null, completedAt: null, ...patch,
      });
      mod.useUploadQueueStore.setState({
        items: [
          row("Kite Season.exfat", "file", "PS5/Kite Season/Kite Season.exfat", {
            status: "done", bytesSent: 3.4e9, totalBytes: 3.4e9, startedAt: now - 1_480_000, completedAt: now - 1_440_000,
          }),
          row("Copper Canyon", "folder", "PS5/Copper Canyon", {
            status: "running", bytesSent: 15.42e9, totalBytes: 41.3e9, bytesPerSec: 109.4e6, startedAt: now - 150_000,
          }),
          row("Lantern Marsh.pkg", "file", "PS5/Lantern Marsh/Lantern Marsh.pkg", { totalBytes: 22.6e9 }),
          row("Frostline (update 01.002).pkg", "file", "PS5/Frostline/Frostline (update 01.002).pkg", { totalBytes: 1.7e9 }),
        ],
        running: true,
        runningHosts: { [host]: true },
      });
    },
    { host: HOST, now: Date.now(), root: ROOT },
  );
}

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
    /** No friend asking to join (a picture about something else on the stream). */
    nobodyAsking?: boolean;
    /** Answers first; undefined falls through to the rest. */
    extra?: (path: string, req: import("@playwright/test").Request) => unknown;
  },
) {
  await page.addInitScript(
    ([host, paired, theme, now, lang, two, start]) => {
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
        JSON.stringify({
          byHost: { [host]: { PPSA90001: 41_820, PPSA90002: 97_260, CUSA90007: 18_540, PPSA90005: 6_300 } },
          lastSeenByHost: {
            [host]: { PPSA90001: now * 1000, PPSA90002: (now - 93_000) * 1000, CUSA90007: (now - 400_000) * 1000, PPSA90005: (now - 900_000) * 1000 },
          },
          lastSampleMs: 0,
        }),
      );
      ls.setItem("ps5upload.fs.lastPath", JSON.stringify({ [host]: start }));
      if (paired) ls.setItem("ps5upload.play_setup.v1", JSON.stringify({ rest: { [host]: true }, played: {}, homeHidden: false }));
    },
    [HOST, opts.paired, opts.theme, NOW, opts.lang, !!opts.two, FILES_START] as const,
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
      if (path.startsWith("/api/join")) {
        const state = joinState(path, !!opts.internet);
        return json(opts.nobodyAsking && "pending" in state ? { ...state, pending: [] } : state);
      }
      if (opts.tailnet) {
        if (path === "/api/ps5/tailnet") return json(TS_CONSOLE);
        if (path === "/api/tailnet") return json(TS_APP);
        if (path === "/api/tailnet/via") return json({ via: "direct", latency_ms: 18.4 });
      }
      switch (path) {
        case "/api/version":
          return json({ version: VERSION, caps: {} });
        case "/api/ps5/status":
          return json({ ok: true, command_count: 1, instance_id: 1, ps5_kernel: `releases/${FIRMWARE}`, ucred_elevated: true, version: VERSION });
        case "/api/ps5/rp/session":
          return json({ host: null });
        // Awake, as the console's own discovery reply says (the power card).
        case "/api/ps5/power/ddp-status":
          return json({
            code: 200, status_text: "Ok", host_id: "000000000000", host_name: "Living Room PS5", host_type: "PS5",
            system_version: "10010000", running_app_name: "", running_app_titleid: "",
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
          return json({ roots: [ROOT], refresh_secs: null, sweep_sidecars: false, trash_available: true });
        case "/api/collection/scan":
          return json({ running: false, deep: false, found: 19, done: 19, started_ms: 0, finished_ms: 0, error: null, games: GAMES.length, locations: 19 });
        case "/api/collection/console":
          return json({ games: consoleGames() });
        case "/api/ps5/apps/installed":
          return json({
            titles: GAMES.filter(([id]) => INSTALLED[id]).map(([id, title, , , , type]) => ({
              title_id: id, title_name: title, origin: type === "pkg" ? "pkg" : "folder", image_backed: type.startsWith("mount."), source: "", system: false,
            })),
            registered_unavailable: false,
            incomplete: false,
          });
        case "/api/games/installed":
          return json(GAMES.filter(([id]) => INSTALLED[id]).map(([id, title]) => ({ title_id: id, title, hosts: [HOST] })));
        case "/api/ps5/saves/list":
          return json({
            // Last saved anywhere from this afternoon to a few weeks ago.
            saves: GAMES.filter(([id]) => INSTALLED[id]).map(([id, , platform], i) => ({
              title_id: id,
              user_id: 0x10000000,
              path: platform === "PS5" ? `/user/home/10000000/savedata_prospero/${id}` : `/user/home/10000000/savedata/${id}`,
              size: [11_534_336, 3_145_728, 26_214_400, 1_048_576, 7_340_032, 2_097_152, 524_288, 5_242_880][i % 8],
              mtime: NOW - [2_700, 94_000, 262_000, 431_000, 780_000, 1_120_000, 1_900_000, 2_650_000][i % 8],
              kind: platform === "PS5" ? "ps5" : "ps4",
            })),
          });
        case "/api/ps5/list-dir": {
          const dir = new URL(route.request().url()).searchParams.get("path") ?? "/data";
          const entries = LISTINGS[dir.replace(/(.)\/$/, "$1")] ?? [];
          return json({ path: dir, entries, truncated: false, total_scanned: entries.length, returned: entries.length });
        }
        case "/api/ps5/files/prefs":
          return json({ supported: true, favorites: [], recent: [], last: null });
        case "/api/ps5/volumes":
          return json({ volumes: VOLUMES });
        case "/api/jobs":
          return json([]);
        // Convert's check of the game folder (#933: a game in asset packs).
        case "/api/fpkg/inspect":
          return json({
            source: `folder ${CONVERT_SOURCE}`,
            files: 1_284,
            bytes: 8.1e9,
            content_id: contentId("PPSA90005", "Pocket Galaxy"),
            title: "Pocket Galaxy",
            planned_size: 11.6e9,
            output_free: 412e9,
            checks: ["eboot.bin", "sce_sys/param.json", "sce_sys/icon0.png", "sce_module"].map((name) => ({ name, ok: true, detail: "ok" })),
            packs: "zstd",
          });
        case "/api/fpkg/estimate":
          return route.fulfill({ status: 500, contentType: "application/json", body: '{"error":"no"}' });
        case "/api/ps5/hw/temps":
          tick++;
          return json({
            cpu_temp: 52 + (tick % 4), soc_temp: 57 + (tick % 3), m2_temp: 43, cpu_freq_mhz: 3500, soc_clock_mhz: 0,
            soc_power_mw: 0, cpu_usage_pct: 18 + (tick % 7), fan_duty_pct: 35, product_shape: 0, fan_pinned_c: 0,
          });
        case "/api/ps5/hw/power":
          return json({
            operating_time_sec: 6_417_480, operating_time_hours: 1782, operating_time_minutes: 38, boot_count: 413,
            power_consumption_mw: 0, load_avg_1m: 0.4, load_avg_5m: 0.3, load_avg_15m: 0.2,
          });
        case "/api/ps5/health/scan":
          return json({
            addr: HOST,
            duration_ms: 612,
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
  await page.route("**/api/version", (route) => route.fulfill(json({ version: VERSION, caps: {} })));
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
            // A queue mid-run, and the same upload in the status bar from here on.
            await uploadQueue(page);
            await runningUpload(page);
            // The queue from its heading down: what runs now, what is next, what is done.
            await page.getByTestId("queue-row").first().evaluate(scrollToY, 288);
            await page.waitForTimeout(600);
          }
          // Games: the filters and the first rows of covers, not the page's heading.
          if (name === "games") {
            const cover = page.locator('main img[src*="/cover"]').first();
            await cover.evaluate(scrollToY, 330);
            await cover.evaluate((el) => {
              const edge = 68; // where the page shows from, under the app's top bar
              let box = el.parentElement;
              while (box && !(box.scrollHeight > box.clientHeight && /auto|scroll/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
              if (!box) return;
              const rows = () =>
                Array.from(document.querySelectorAll("main button, main input, main select"))
                  .map((b) => b.getBoundingClientRect())
                  .filter((r) => r.height > 0 && r.top < edge + 2 && r.bottom > edge);
              // A row cut through: show all of it. Only its last few pixels left: hide those.
              const cut = rows().filter((r) => r.bottom > edge + 12);
              if (cut.length) box.scrollTop -= edge + 12 - Math.min(...cut.map((r) => r.top));
              const slivers = rows().filter((r) => r.bottom <= edge + 12);
              if (slivers.length) box.scrollTop += Math.max(...slivers.map((r) => r.bottom)) - edge + 4;
            });
            await coversLoaded(page);
            await page.waitForTimeout(400);
          }
          // Workshop: a game picked and checked, as it is just before a conversion.
          if (name === "workshop") {
            const source = page.getByRole("main").locator("#fpkg-source");
            await source.fill(CONVERT_SOURCE);
            await source.blur();
            await expect(page.getByRole("main").getByTestId("unpack-option")).toBeVisible({ timeout: 30_000 });
            await page.mouse.move(8, 8);
            await page.waitForTimeout(600);
          }
          await page.mouse.move(8, 8);
          await page.waitForTimeout(250);
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

    // 12.6.31: a picked package's "Name on the PS5", and 12.6.30: Convert unpacking asset packs.
    // Both are a part of the screen, close up: a narrow window, so the words stay readable in a
    // small picture beside the site's text.
    test.describe(`${lang} close-ups ${theme}`, () => {
      test.use({ viewport: { width: 640, height: 1000 }, deviceScaleFactor: 2 });

      test(`the package name card (${theme})`, async ({ page }) => {
        const file = `${RENAMED}.pkg`;
        const id = contentId("PPSA90010", RENAMED);
        await fakeConsole(page, {
          paired: true,
          theme,
          lang,
          extra: (path) => {
            if (path === "/api/files/upload") return { upload_id: "up-1", path: `${ROOT}/PS5/${RENAMED}/${file}`, size: 22.6e9, filename: file };
            if (path === "/api/pkg/parse-split") return { head: { content_id: id, title: RENAMED, category: "gd", platform: "ps5" }, parts: [], total_size: 22.6e9 };
            if (path === "/api/pkg/name")
              return {
                platform: "ps5", content_id: id, name: RENAMED,
                entries: [{ key: "en-US", name: RENAMED }, { key: "fr-FR", name: "Le Marais aux lanternes" }],
                other_names: 1, refusal: null, max_bytes: 127,
              };
            return undefined;
          },
        });
        await page.goto("/send", { waitUntil: "domcontentloaded" });
        const picker = page.locator('input[type="file"][multiple]').first();
        await picker.waitFor({ state: "attached", timeout: 30_000 });
        await picker.setInputFiles({ name: file, mimeType: "application/octet-stream", buffer: Buffer.from("made-up package") });
        const card = page.getByTestId("pkg-name-card");
        await expect(card).toBeVisible({ timeout: 30_000 });
        await card.getByTestId("pkg-name-input").fill(`${RENAMED}: Deluxe`);
        await expect(card.getByTestId("pkg-name-save")).toBeEnabled();
        await card.evaluate(scrollToY, 200);
        await page.mouse.move(8, 8);
        await page.waitForTimeout(500);
        await shotAround(page, [card], `${dir}/pkg-name-${theme}.png`);
      });

      test(`convert unpacks asset packs (${theme})`, async ({ page }) => {
        await fakeConsole(page, { paired: true, theme, lang });
        await page.goto("/workshop", { waitUntil: "domcontentloaded" });
        const main = page.getByRole("main");
        const source = main.locator("#fpkg-source");
        await source.waitFor({ timeout: 30_000 });
        await source.fill(CONVERT_SOURCE);
        await source.blur();
        const unpack = main.getByTestId("unpack-option");
        await expect(unpack.getByRole("checkbox")).toBeChecked({ timeout: 30_000 });
        await unpack.evaluate(scrollToY, 240);
        await page.mouse.move(8, 8);
        await page.waitForTimeout(500);
        await shotAround(page, [unpack, main.getByTestId("leave-out-ampr-emu")], `${dir}/convert-unpack-${theme}.png`, { x: 12, y: 3 });
      });
    });

    // 12.6.30: the Android app on a TV (MainActivity's bridge says so): a new install's first
    // screen offers Join Play Together, in focus for the remote.
    test.describe(`${lang} tv ${theme}`, () => {
      test.use({ viewport: { width: 960, height: 540 }, deviceScaleFactor: 2 });

      test(`the app on a TV (${theme})`, async ({ page }) => {
        await page.addInitScript(
          ([lang, theme]) => {
            (window as unknown as { PS5UploadTv: { isTv(): boolean } }).PS5UploadTv = { isTv: () => true };
            const ls = window.localStorage;
            ls.setItem("ps5upload.lang", lang);
            ls.setItem("ps5upload.theme", theme);
            ls.setItem("ps5upload.join.name", "Sam");
            ls.setItem("ps5upload.join.guided", "1");
          },
          [lang, theme] as const,
        );
        await page.route(
          (url) => url.pathname.startsWith("/api/"),
          (route) => {
            const path = new URL(route.request().url()).pathname;
            const body = path === "/api/version" ? { version: VERSION, caps: {} } : path === "/api/ps5/rp/session" ? { host: null } : {};
            return route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
          },
        );
        await page.goto("/", { waitUntil: "domcontentloaded" });
        await expect(page.getByTestId("tv-join-go")).toBeFocused({ timeout: 30_000 });
        await page.waitForTimeout(800);
        await shot(page, "tv-join", theme);
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
        await fakeConsole(page, { paired: true, theme, lang, nobodyAsking: true });
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
        await fakeConsole(page, { paired: true, theme, lang, nobodyAsking: true });
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
        await fakeConsole(page, { paired: true, theme, lang, nobodyAsking: true });
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
        await fakeConsole(page, { paired: true, theme, lang, nobodyAsking: true });
        await page.goto("/home", { waitUntil: "domcontentloaded" });
        await expect(page.getByTestId("home-play")).toBeVisible({ timeout: 30_000 });
        await page.waitForTimeout(1500);
        await shot(page, "m-home", theme);
        await page.goto("/games", { waitUntil: "domcontentloaded" });
        await page.waitForTimeout(2500);
        // Down to the covers, the first row whole under the app's bar.
        await page.locator('main img[src*="/cover"]').first().evaluate(scrollToY, 84);
        await coversLoaded(page);
        await page.waitForTimeout(600);
        await shot(page, "m-games", theme);
        // 12.6.30: Files on a phone. A long press starts selecting; the actions are at the bottom.
        await page.goto("/files", { waitUntil: "domcontentloaded" });
        const fileRow = (name: string) => page.getByRole("main").locator(`li[data-fs-name="${name}"]`);
        await expect(fileRow("Kite Season.exfat")).toBeVisible({ timeout: 30_000 });
        await longPress(page, fileRow("Kite Season.exfat"));
        await expect(page.getByRole("main").getByRole("toolbar")).toBeVisible({ timeout: 10_000 });
        await fileRow("Lantern Marsh.pkg").tap();
        await page.waitForTimeout(600);
        await shot(page, "m-files", theme);
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
