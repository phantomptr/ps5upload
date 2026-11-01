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

/** The made-up console, as the engine would answer for it. */
async function fakeConsole(page: Page, opts: { paired: boolean; theme: "light" | "dark"; lang: string }) {
  await page.addInitScript(
    ([host, paired, theme, now, lang]) => {
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
    [HOST, opts.paired, opts.theme, NOW, opts.lang] as const,
  );
  await page.route("**/api/collection/games/*/cover*", (route) => {
    const id = new URL(route.request().url()).pathname.split("/")[4];
    return route.fulfill({ contentType: "image/png", body: art(id) });
  });
  await page.route("**/api/ps5/app-icon*", (route) => {
    const id = new URL(route.request().url()).searchParams.get("title_id") ?? "PPSA90001";
    return route.fulfill({ contentType: "image/png", body: art(GAMES.some((g) => g[0] === id) ? id : "PPSA90001") });
  });
  let tick = 0;
  await page.route(
    (url) => url.pathname.startsWith("/api/") && !url.pathname.includes("/cover") && !url.pathname.includes("app-icon"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      const json = (b: unknown) => route.fulfill({ contentType: "application/json", body: JSON.stringify(b) });
      switch (path) {
        case "/api/version":
          return json({ version: "10.0.0", caps: {} });
        case "/api/ps5/status":
          return json({ ok: true, command_count: 1, instance_id: 1, ps5_kernel: "releases/10.00", ucred_elevated: true });
        case "/api/ps5/rp/session":
          return json({ host: null });
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
  await page.route("**/api/version", (route) => route.fulfill(json({ version: "10.4.0", caps: {} })));
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

    });
  }
}
