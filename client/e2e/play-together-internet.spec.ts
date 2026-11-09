import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { createServer as netServer, type AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";

// Play Together over the internet, the whole way on this computer: the real hub binary
// (ps5upload-hub), the engine's Play Together (its real routes, join_net's WebRTC, the invite
// link) with a pretend session on a pretend console (join_net/join_tests.rs
// e2e_internet_guest_fixture: the fakes join.rs's own tests use, no PS5), and the guest page as
// the website serves it (site/join/index.html from a local static server). The guest's browser
// signals through the hub and reaches the engine peer to peer over WebRTC. Nothing reaches the
// internet: no STUN, no router mapping (ICE on this computer's own addresses).
//
// Needs cargo, so it runs only when asked: PS5UPLOAD_E2E_INTERNET=1 npx playwright test
// play-together-internet (it builds the hub and the engine's test binary first).

const RUN = !!process.env.PS5UPLOAD_E2E_INTERNET;
const ENGINE_DIR = fileURLToPath(new URL("../../engine/", import.meta.url));
const SITE_PAGE = fileURLToPath(new URL("../../site/join/index.html", import.meta.url));
const SITE = "https://ps5upload.phantomptr.com/join/";

test.describe.configure({ mode: "serial" });
test.skip(!RUN, "starts the real hub and the engine's test build: PS5UPLOAD_E2E_INTERNET=1");

const children: ChildProcess[] = [];
let site: Server;
let siteOrigin = "";
const siteRequests: string[] = [];
let hubPort = 0;
let engine = "";
let consoleAddr = "";

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

type State = {
  pending?: Array<{ id: number; name: string }>;
  guests?: Array<{ player: number; via: string; path?: string; state: string }>;
  invite?: { internet?: { state?: string; url?: string } };
};
const state = async () => (await api(`/api/join?addr=${consoleAddr}`)) as State;

test.beforeAll(async () => {
  if (!RUN) return;
  test.setTimeout(20 * 60_000);
  // The binaries: the hub, and the engine's test build that holds the fixture.
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
    if (req.method === "GET" && req.url === "/join/") {
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
  const eng = spawn(exe, ["join::tests::internet::e2e_internet_guest_fixture", "--exact", "--ignored", "--nocapture"], {
    env: { ...process.env, PS5UPLOAD_E2E_HUB: `ws://127.0.0.1:${hubPort}/ws`, RUST_LOG: "info" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(eng);
  const m = await waitLine(eng, /E2E_ENGINE=(\S+) E2E_CONSOLE=(\S+)/, 60_000);
  engine = m[1];
  consoleAddr = m[2];
});

test.afterAll(async () => {
  if (engine) await fetch(`${engine}/e2e/stop`, { method: "POST" }).catch(() => {});
  for (const c of children) {
    // By PID: the processes this spec started, nothing else.
    if (c.pid && c.exitCode === null) process.kill(c.pid, "SIGTERM");
  }
  await new Promise<void>((r) => (site ? site.close(() => r()) : r()));
});

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

test("a friend anywhere joins through the hub, is allowed, plays as Player 2 and sees the picture", async ({ page }) => {
  test.setTimeout(120_000);
  const logs: string[] = [];
  page.on("console", (m) => logs.push(m.text()));
  // The internet invite: a room on the hub, the website's link with the key and the fingerprint.
  await api("/api/join/open", { addr: consoleAddr });
  const inv = (await api("/api/join/invite", { addr: consoleAddr, internet: true })) as { key: string; internet: { url: string; room: string } };
  const link = inv.internet.url;
  expect(link.startsWith(`${SITE}#room=${inv.internet.room}&k=${inv.key}&fp=`)).toBe(true);
  expect(link).toContain(`&hub=127.0.0.1:${hubPort}`);
  await expect.poll(async () => (await state()).invite?.internet?.state, { timeout: 15_000 }).toBe("hosted");

  // The same link, from the local copy of the website.
  await page.goto(link.replace(SITE, `${siteOrigin}/join/`), { waitUntil: "domcontentloaded" });
  // The app joins internet invites too: the page offers it first; this test plays in the browser.
  await expect(page.getByTestId("open-in-gate")).toBeVisible({ timeout: 15_000 });
  await page.getByTestId("open-in-browser").first().click();
  await page.getByTestId("join-name").fill("Ben");
  await page.getByTestId("join-go").click();
  await expect(page.getByTestId("join-stage")).toContainText("to let you in", { timeout: 30_000 });
  expect(logs.some((l) => l.includes("[join] channel open")), logs.join("\n")).toBe(true);

  // The host allows them through the engine's API.
  await expect.poll(async () => (await state()).pending?.[0]?.name, { timeout: 10_000 }).toBe("Ben");
  const id = (await state()).pending![0].id;
  await api("/api/join/decide", { id, allow: true });
  await expect(page.getByTestId("join-player")).toHaveText("You are Player 2", { timeout: 15_000 });
  await expect
    .poll(async () => {
      const g = (await state()).guests?.[0];
      return g ? `${g.via} ${g.path}` : "";
    })
    .toBe("internet direct");
  // One invite: the browser and the engine share this computer's network, and ICE keeps the
  // connection on it, though Chrome hides its own address behind an mDNS name the engine
  // ignores (the engine learns it from Chrome's checks: a peer-reflexive candidate).
  await expect
    .poll(async () => ((await state()).guests?.[0] as { link?: { path?: string } } | undefined)?.link?.path, { timeout: 10_000 })
    .toBe("lan");
  expect(link).toContain("&lan=");

  // The picture: the session's stream reaches the guest over the data channel (the keyframe in
  // pieces) and draws.
  const stream = await makeStream(page);
  for (const msg of stream) {
    const r = await fetch(`${engine}/e2e/stream`, { method: "POST", body: Uint8Array.from(msg) });
    expect(r.status).toBe(204);
  }
  await expect
    .poll(() => page.getByTestId("watch-canvas").evaluate((c) => (c as HTMLCanvasElement).width), { timeout: 15_000 })
    .toBe(320);

  // The page asked its own origin for nothing but itself: no /api, no icon, no strings.
  expect(siteRequests).toEqual(["GET /join/"]);

  await page.getByTestId("join-leave").click();
  await expect(page.getByTestId("join-name-step")).toBeVisible();
  await expect.poll(async () => (await state()).guests?.[0]?.state, { timeout: 10_000 }).toBe("left");
  await api("/api/join/close", { addr: consoleAddr });
});

test("the invite service can't be reached: the page offers the host's own page on the Wi-Fi", async ({ page }) => {
  test.setTimeout(60_000);
  // An internet link whose hub nothing answers (a port just closed), naming the engine's page as
  // the Wi-Fi's (as a newer engine's link does).
  const dead = await freePort();
  const key = "0f".repeat(16);
  const fp = Array.from({ length: 32 }, () => "AB").join(":");
  const lan = `${engine}/join`;
  const link = `${siteOrigin}/join/#room=${"r".repeat(22)}&k=${key}&fp=${fp}&hub=127.0.0.1:${dead}&lan=${encodeURIComponent(lan)}`;
  await page.goto(link, { waitUntil: "domcontentloaded" });
  // Before the friend types anything: a few seconds, then the way in on the Wi-Fi.
  const open = page.getByTestId("open-on-wifi-link");
  await expect(open).toBeVisible({ timeout: 10_000 });
  await expect(open).toHaveAttribute("href", `${lan}#i=${key}`);
  // A tap goes there: the engine's own guest page, with the same invite key.
  await open.click();
  await expect(page).toHaveURL(`${lan}#i=${key}`);
  // (A LAN link in a browser first offers the app.)
  await expect(page.getByTestId("open-in-gate").or(page.getByTestId("join-name-step"))).toBeVisible();
});
