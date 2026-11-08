import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { createServer as netServer, type AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";

// The phone panel's Internet links, the whole way on this computer: the real hub binary
// (ps5upload-hub), the engine's phone link (its real routes, join_net's WebRTC, the link) with a
// pretend Remote Play session (join_net/pad_tests.rs e2e_pad_internet_fixture: no PS5), and the
// phone page as the website serves it (site/pad/index.html from a local static server). The
// phone's browser signals through the hub and reaches the engine peer to peer over WebRTC, as a
// controller (its buttons reach the console's pad, the console's rumble buzzes it) and as a
// watcher (the picture over the data channel). Nothing reaches the internet: no STUN, no router
// mapping (ICE on this computer's own addresses).
//
// Needs cargo, so it runs only when asked: PS5UPLOAD_E2E_INTERNET=1 npx playwright test
// phone-pad-internet (it builds the hub and the engine's test binary first).

const RUN = !!process.env.PS5UPLOAD_E2E_INTERNET;
const ENGINE_DIR = fileURLToPath(new URL("../../engine/", import.meta.url));
const SITE_PAGE = fileURLToPath(new URL("../../site/pad/index.html", import.meta.url));
const SITE = "https://ps5upload.phantomptr.com/pad/";

test.describe.configure({ mode: "serial" });
test.skip(!RUN, "starts the real hub and the engine's test build: PS5UPLOAD_E2E_INTERNET=1");
// A phone: touch, sideways.
test.use({ viewport: { width: 844, height: 390 }, hasTouch: true, isMobile: true });

const children: ChildProcess[] = [];
let site: Server;
let siteOrigin = "";
const siteRequests: string[] = [];
let hubPort = 0;
let engine = "";

function cargo(args: string[]): string {
  const r = spawnSync("cargo", args, { cwd: ENGINE_DIR, encoding: "utf8", maxBuffer: 256 << 20 });
  if (r.status !== 0) throw new Error(`cargo ${args.join(" ")} failed:\n${r.stderr.slice(-4000)}`);
  return r.stdout;
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = netServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as AddressInfo).port;
      s.close(() => resolve(p));
    });
  });
}

/** Waits for a child's stdout line matching `re` (it prints it when ready). */
function waitLine(child: ChildProcess, re: RegExp, ms: number): Promise<RegExpMatchArray> {
  return new Promise((resolve, reject) => {
    let buf = "";
    const t = setTimeout(() => reject(new Error(`no ${re} in ${ms} ms:\n${buf.slice(-2000)}`)), ms);
    child.stdout?.on("data", (d: Buffer) => {
      buf += d.toString();
      const m = re.exec(buf);
      if (m) {
        clearTimeout(t);
        resolve(m);
      }
    });
    child.once("exit", (code) => {
      clearTimeout(t);
      reject(new Error(`exited ${code}:\n${buf.slice(-2000)}`));
    });
  });
}

async function api(path: string, body?: unknown): Promise<Record<string, unknown>> {
  const r = await fetch(`${engine}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${path}: HTTP ${r.status} ${await r.text()}`);
  return (await r.json()) as Record<string, unknown>;
}

type Link = {
  internet?: { url?: string; state?: string } | null;
  pads?: Array<{ id: number; watch: boolean; via: string }>;
  pending?: Array<{ id: number; via: string; watch: boolean }>;
};

/** The host answers the internet phone asking (as Allow / No over the picture do). */
async function answer(allow: boolean, watch: boolean): Promise<void> {
  await expect.poll(async () => (await link()).pending?.map((p) => `${p.via} ${p.watch}`), { timeout: 30_000 }).toEqual([`internet ${watch}`]);
  const id = (await link()).pending![0].id;
  expect((await api("/api/ps5/rp/pad-link/decide", { id, allow })).ok).toBe(true);
}
const link = async () => (await api("/api/ps5/rp/pad-link")) as Link;
const consolePad = async () => (await api("/e2e/pad")) as { buttons: number };

test.beforeAll(async () => {
  if (!RUN) return;
  test.setTimeout(20 * 60_000);
  cargo(["build", "-p", "ps5upload-hub"]);
  const exe = cargo(["test", "-p", "ps5upload-engine", "--lib", "--no-run", "--message-format=json"])
    .split("\n")
    .filter((l) => l.startsWith("{"))
    .map((l) => JSON.parse(l) as { reason?: string; target?: { name?: string }; executable?: string | null })
    .find((m) => m.reason === "compiler-artifact" && m.target?.name === "ps5upload_engine" && m.executable)?.executable;
  if (!exe) throw new Error("no engine test binary");

  // The website: the one page, and a list of everything asked of it.
  const page = readFileSync(SITE_PAGE);
  site = createServer((req, res) => {
    siteRequests.push(`${req.method} ${req.url}`);
    if (req.method === "GET" && req.url === "/pad/") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(page);
      return;
    }
    res.writeHead(404);
    res.end();
  });
  await new Promise<void>((r) => site.listen(0, "127.0.0.1", r));
  siteOrigin = `http://127.0.0.1:${(site.address() as AddressInfo).port}`;

  // The hub, letting only this page's origin in (as it lets only the website's in production).
  hubPort = await freePort();
  const hub = spawn(`${ENGINE_DIR}target/debug/ps5upload-hub`, [], {
    env: { ...process.env, HUB_HTTP_LISTEN: `127.0.0.1:${hubPort}`, HUB_ALLOWED_ORIGINS: siteOrigin },
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(hub);
  await expect
    .poll(async () => (await fetch(`http://127.0.0.1:${hubPort}/health`).then((r) => r.text()).catch(() => "")).trim(), { timeout: 15_000 })
    .toBe("ok");

  // The engine, its settings pointing at that hub.
  const eng = spawn(exe, ["join::tests::pad_internet::e2e_pad_internet_fixture", "--exact", "--ignored", "--nocapture"], {
    env: { ...process.env, PS5UPLOAD_E2E_HUB: `ws://127.0.0.1:${hubPort}/ws`, RUST_LOG: "info" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(eng);
  const m = await waitLine(eng, /E2E_ENGINE=(\S+)/, 60_000);
  engine = m[1];
});

test.afterAll(async () => {
  if (engine) await fetch(`${engine}/e2e/stop`, { method: "POST" }).catch(() => {});
  for (const c of children) {
    // By PID: the processes this spec started, nothing else.
    if (c.pid && c.exitCode === null) process.kill(c.pid, "SIGTERM");
  }
  await new Promise<void>((r) => (site ? site.close(() => r()) : r()));
});

/** The internet link (made once; the same one after), hosted on the hub. */
async function internetLink(): Promise<string> {
  const s = (await api("/api/ps5/rp/pad-link/internet", {})) as Link;
  const url = s.internet?.url ?? "";
  expect(url.startsWith(`${SITE}#room=`)).toBe(true);
  expect(url).toContain(`&hub=127.0.0.1:${hubPort}`);
  await expect.poll(async () => (await link()).internet?.state, { timeout: 15_000 }).toBe("hosted");
  return url.replace(SITE, `${siteOrigin}/pad/`);
}

/** One second of 320x240 H.264 as the engine forwards it: [kind, key, ...data]. */
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
    return [[0, 1, ...first.subarray(0, idr)], [1, 1, ...first.subarray(idr)], ...aus.slice(1).map((a) => [1, 0, ...a])];
  });
}

test("a phone anywhere is a controller: its press reaches the console, the console's rumble buzzes it", async ({ page }) => {
  test.setTimeout(120_000);
  const logs: string[] = [];
  page.on("console", (m) => logs.push(m.text()));
  // The phone's motor, as the page drives it (no real buzz in a test browser).
  await page.addInitScript(() => {
    const w = window as unknown as { vibrations: unknown[] };
    w.vibrations = [];
    Object.defineProperty(navigator, "vibrate", { configurable: true, value: (p: unknown) => (w.vibrations.push(p), true) });
  });
  const url = await internetLink();
  await page.goto(url, { waitUntil: "domcontentloaded" });
  // An internet link plays in the browser: no "Open in the app?" question.
  await expect(page.getByTestId("open-in-gate")).toHaveCount(0);
  // Over the internet the host allows the phone first: no controller until then.
  await expect(page.getByTestId("pad-stage")).toHaveText("Waiting for the computer to allow this phone…", { timeout: 30_000 });
  const cross = page.getByRole("button", { name: "✕" });
  await expect(cross).toHaveCount(0);
  expect((await link()).pads).toEqual([]);
  await answer(true, false);
  await expect(cross).toBeVisible({ timeout: 30_000 });
  expect(logs.some((l) => l.includes("[pad] channel open")), logs.join("\n")).toBe(true);
  await expect.poll(async () => (await link()).pads?.map((p) => `${p.via} ${p.watch}`)).toEqual(["internet false"]);

  // Held down: the console's pad has ✕ (bit 0) until the finger lifts.
  const box = (await cross.boundingBox())!;
  const cdp = await page.context().newCDPSession(page);
  const at = [{ x: box.x + box.width / 2, y: box.y + box.height / 2 }];
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: at });
  await expect.poll(async () => (await consolePad()).buttons & 1, { timeout: 10_000 }).toBe(1);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await expect.poll(async () => (await consolePad()).buttons & 1, { timeout: 10_000 }).toBe(0);

  // The console's rumble comes back over the channel.
  await fetch(`${engine}/e2e/rumble`, { method: "POST", body: JSON.stringify({ rumble: [200, 0] }) });
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { vibrations: unknown[] }).vibrations.length), { timeout: 10_000 })
    .toBeGreaterThan(0);

  // The page asked its own origin for nothing but itself.
  expect(siteRequests.every((r) => r === "GET /pad/")).toBe(true);
  await page.close();
  await expect.poll(async () => (await link()).pads?.length, { timeout: 15_000 }).toBe(0);
});

test("a watcher anywhere sees the picture over the data channel and has no controller", async ({ page }) => {
  test.setTimeout(120_000);
  const url = await internetLink();
  siteRequests.length = 0;
  await page.goto(`${url}&w=1`, { waitUntil: "domcontentloaded" });
  // The host says No: the page says so, with Try again.
  await answer(false, true);
  await expect(page.getByTestId("pad-stage")).toHaveText("The computer said no.", { timeout: 30_000 });
  await page.getByTestId("pad-retry").tap();
  // Asked again, allowed: watching.
  await answer(true, true);
  await expect(page.getByTestId("pad-stage")).toContainText("Watching", { timeout: 30_000 });
  await expect.poll(async () => (await link()).pads?.map((p) => `${p.via} ${p.watch}`)).toEqual(["internet true"]);
  // The stream (made in this browser), as the console's session hands it to watchers.
  const stream = await makeStream(page);
  for (const msg of stream) {
    const r = await fetch(`${engine}/e2e/stream`, { method: "POST", body: Uint8Array.from(msg) });
    expect(r.status).toBe(204);
  }
  await expect
    .poll(() => page.getByTestId("watch-canvas").evaluate((c) => (c as HTMLCanvasElement).width), { timeout: 15_000 })
    .toBe(320);
  await expect(page.getByTestId("watch-problem")).toHaveCount(0);
  // Over the internet a watcher only watches.
  await expect(page.getByRole("button", { name: "✕" })).toHaveCount(0);
  const id = (await link()).pads![0].id;
  expect((await api("/api/ps5/rp/pad-link/control", { id, on: true })).ok).toBe(false);
  expect(siteRequests).toEqual(["GET /pad/"]);

  // New link: the old one stops working (a fresh page on it is told so).
  const fresh = ((await api("/api/ps5/rp/pad-link/internet", { new: true })) as Link).internet?.url;
  expect(fresh).toBeTruthy();
  expect(fresh).not.toBe(url.replace(`${siteOrigin}/pad/`, SITE));
  const old = await page.context().newPage();
  await old.goto(url, { waitUntil: "domcontentloaded" });
  await expect(old.getByTestId("pad-stage")).toContainText("expired", { timeout: 30_000 });
  await api("/api/ps5/rp/pad-link", { open: false });
});
