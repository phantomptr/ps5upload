import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { createServer as netServer, type AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

// Play Rooms over the internet, the whole way on this computer: the real hub binary, the engine's
// Play Together with its lobby on (join_net/join_tests.rs e2e_internet_guest_fixture with
// PS5UPLOAD_E2E_LOBBY: a pretend session on a pretend console, no PS5), and the website's pages
// (site/join/, site/rooms/) from a local static server. A Public room with a password: its card
// (made with the hub's proof of work) in the hub's list, read by the website's origin (CORS) and
// by no other; the details relayed by the hub from the host engine; the guest page's knock with
// the password sealed to the room key; the host lets them in; WebRTC to Player 2.
//
// With PS5UPLOAD_E2E_HUB_BIN naming a hub from before rooms (12.6.46) and PS5UPLOAD_E2E_HUB_OLD=1,
// the other test runs instead: the same room on an old hub says "no public rooms", the lobby
// waits quietly, and an internet invite still works (engine/crates/ps5upload-hub/COMPAT.md).
//
// Needs cargo, so it runs only when asked: PS5UPLOAD_E2E_INTERNET=1 npx playwright test
// play-rooms-internet (it builds the hub and the engine's test binary first).

const RUN = !!process.env.PS5UPLOAD_E2E_INTERNET;
const OLD_HUB = !!process.env.PS5UPLOAD_E2E_HUB_OLD;
const ENGINE_DIR = fileURLToPath(new URL("../../engine/", import.meta.url));
const SITE_JOIN = fileURLToPath(new URL("../../site/join/index.html", import.meta.url));
const SITE_ROOMS = fileURLToPath(new URL("../../site/rooms/index.html", import.meta.url));
const SITE = "https://ps5upload.phantomptr.com/join/";

test.describe.configure({ mode: "serial" });
test.skip(!RUN, "starts the real hub and the engine's test build: PS5UPLOAD_E2E_INTERNET=1");

const children: ChildProcess[] = [];
const servers: Server[] = [];
let siteOrigin = "";
let otherOrigin = "";
let hubPort = 0;
let hubLog = "";
let engine = "";
let engineLog = "";
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
  pending?: Array<{ id: number; name: string; password_ok?: boolean }>;
  guests?: Array<{ player: number; via: string; path?: string; state: string }>;
  invite?: { internet?: { state?: string; url?: string } };
  room?: { listed?: { state?: string; why?: string | null } };
};
const state = async () => (await api(`/api/join?addr=${consoleAddr}`)) as State;

/** A GET to the hub with an Origin (as a browser on that origin sends it). */
async function hubGet(path: string, origin?: string): Promise<{ status: number; acao: string | null; body: string }> {
  const r = await fetch(`http://127.0.0.1:${hubPort}${path}`, { headers: origin ? { origin } : {} });
  return { status: r.status, acao: r.headers.get("access-control-allow-origin"), body: await r.text() };
}

/** The hub's counters (its /metrics on the same port: no HUB_METRICS_LISTEN here). */
async function hubMetrics(): Promise<{ hosts: number; rooms: number; errors: Record<string, number> }> {
  const text = await fetch(`http://127.0.0.1:${hubPort}/metrics`).then((r) => r.text());
  const one = (name: string) => Number(new RegExp(`^${name} (\\d+)$`, "m").exec(text)?.[1] ?? "0");
  const errors: Record<string, number> = {};
  for (const m of text.matchAll(/^hub_errors_total\{kind="([a-z_]+)"\} (\d+)$/gm)) {
    if (Number(m[2]) > 0) errors[m[1]] = Number(m[2]);
  }
  return { hosts: one("hub_hosts_total"), rooms: one("hub_rooms"), errors };
}

function staticSite(pages: Record<string, Buffer>): Promise<string> {
  const srv = createServer((req, res) => {
    const page = req.method === "GET" ? pages[req.url ?? ""] : undefined;
    if (page) {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(page);
      return;
    }
    res.writeHead(404);
    res.end();
  });
  servers.push(srv);
  return new Promise((r) => srv.listen(0, "127.0.0.1", () => r(`http://127.0.0.1:${(srv.address() as AddressInfo).port}`)));
}

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

  siteOrigin = await staticSite({ "/join/": readFileSync(SITE_JOIN), "/rooms/": readFileSync(SITE_ROOMS) });
  otherOrigin = await staticSite({ "/": Buffer.from("<!doctype html><title>other</title>") });

  // The hub, letting only the website's (local) origin in. Real proof of work (the default).
  hubPort = await freePort();
  const hub = spawn(process.env.PS5UPLOAD_E2E_HUB_BIN || `${ENGINE_DIR}target/debug/ps5upload-hub`, [], {
    env: { ...process.env, HUB_HTTP_LISTEN: `127.0.0.1:${hubPort}`, HUB_ALLOWED_ORIGINS: siteOrigin, HUB_SUMMARY_SECS: "5" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(hub);
  hub.stdout?.on("data", (d: Buffer) => (hubLog += d.toString()));
  hub.stderr?.on("data", (d: Buffer) => (hubLog += d.toString()));
  await expect
    .poll(async () => (await fetch(`http://127.0.0.1:${hubPort}/health`).then((r) => r.text()).catch(() => "")).trim(), { timeout: 15_000 })
    .toBe("ok");

  const eng = spawn(exe, ["join::tests::internet::e2e_internet_guest_fixture", "--exact", "--ignored", "--nocapture"], {
    env: { ...process.env, PS5UPLOAD_E2E_HUB: `ws://127.0.0.1:${hubPort}/ws`, PS5UPLOAD_E2E_LOBBY: "1", RUST_LOG: "debug" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(eng);
  eng.stdout?.on("data", (d: Buffer) => (engineLog += d.toString()));
  eng.stderr?.on("data", (d: Buffer) => (engineLog += d.toString()));
  const m = await waitLine(eng, /E2E_ENGINE=(\S+) E2E_CONSOLE=(\S+)/, 60_000);
  engine = m[1];
  consoleAddr = m[2];
});

test.afterAll(async () => {
  if (engine) await fetch(`${engine}/e2e/stop`, { method: "POST" }).catch(() => {});
  for (const c of children) {
    if (c.pid && c.exitCode === null) process.kill(c.pid, "SIGTERM");
  }
  await Promise.all(servers.map((s) => new Promise<void>((r) => s.close(() => r()))));
});

async function openPublicRoom(): Promise<void> {
  await api("/api/join/open", { addr: consoleAddr });
  await api("/api/join/room", { addr: consoleAddr, visibility: "public", name: "Couch chaos", password: "couch42", public_ok: true });
}

test("a public room: listed with proof of work, read by the website's origin only, details from the host, a sealed knock, Player 2", async ({ page }) => {
  test.skip(OLD_HUB, "a hub with rooms");
  test.setTimeout(180_000);
  const logs: string[] = [];
  page.on("console", (m) => logs.push(m.text()));
  await openPublicRoom();

  // The card reaches the hub's list (the engine solved the hub's proof of work).
  let card: { room: string; name: string; pw: boolean; tag: string; host: string } | undefined;
  await expect
    .poll(
      async () => {
        const r = await hubGet("/rooms", siteOrigin);
        card = (JSON.parse(r.body) as { rooms: Array<NonNullable<typeof card>> }).rooms.find((x) => x.name === "Couch chaos");
        return !!card;
      },
      { timeout: 30_000 },
    )
    .toBe(true);
  await expect.poll(async () => (await state()).room?.listed?.state, { timeout: 10_000 }).toBe("listed");
  expect(card!.pw).toBe(true);
  // No address anywhere in the list.
  const list = await hubGet("/rooms", siteOrigin);
  expect(list.body).not.toMatch(/\b(?:\d{1,3}\.){3}\d{1,3}\b/);

  // CORS: the website's origin gets the header, no Origin (an app) gets the list without it, any
  // other origin is refused.
  expect(list.status).toBe(200);
  expect(list.acao).toBe(siteOrigin);
  const app = await hubGet("/rooms");
  expect(app.status).toBe(200);
  expect(app.acao).toBeNull();
  expect((await hubGet("/rooms", "https://evil.example")).status).toBe(403);
  // The same in a real browser: the website's page may read it, another site's may not.
  await page.goto(`${siteOrigin}/rooms/`, { waitUntil: "domcontentloaded" });
  const fromSite = await page.evaluate(async (u) => {
    try {
      const r = await fetch(u);
      return `${r.status} ${(await r.text()).includes("Couch chaos")}`;
    } catch (e) {
      return `error ${String(e)}`;
    }
  }, `http://127.0.0.1:${hubPort}/rooms`);
  expect(fromSite).toBe("200 true");
  const other = await page.context().newPage();
  await other.goto(`${otherOrigin}/`);
  const fromOther = await other.evaluate(async (u) => {
    try {
      const r = await fetch(u);
      return `${r.status}`;
    } catch {
      return "blocked";
    }
  }, `http://127.0.0.1:${hubPort}/rooms`);
  expect(fromOther).toBe("blocked");
  await other.close();

  // The website's Rooms page shows the card (its hub, hub.phantomptr.com, answered by this one).
  await page.route("https://hub.phantomptr.com/**", async (route) => {
    const u = new URL(route.request().url());
    const r = await route.fetch({ url: `http://127.0.0.1:${hubPort}${u.pathname}${u.search}` });
    await route.fulfill({ response: r });
  });
  await page.goto(`${siteOrigin}/rooms/`, { waitUntil: "domcontentloaded" });
  await expect(page.getByText("Couch chaos").first()).toBeVisible({ timeout: 15_000 });
  await page.unroute("https://hub.phantomptr.com/**");

  // Details: the hub relays the host engine's preview; the engine's own proxy has the list too.
  const preview = await hubGet(`/rooms/${card!.room}/preview`, siteOrigin);
  expect(preview.status, preview.body).toBe(200);
  expect(preview.body).toContain("Couch chaos");
  expect(preview.body).not.toMatch(/\b(?:\d{1,3}\.){3}\d{1,3}\b/);
  const viaEngine = await fetch(`${engine}/api/join/rooms`).then((r) => r.text());
  expect(viaEngine).toContain("Couch chaos");

  // The guest page from the room's card: name and password, the knock (sealed to the room key),
  // the host lets them in, WebRTC through the hub's signalling.
  const link = `${siteOrigin}/join/#lobby=${card!.room}&hub=127.0.0.1:${hubPort}&via=rooms&n=${encodeURIComponent(card!.host)}&tag=${card!.tag}&pw=1`;
  await page.goto(link, { waitUntil: "domcontentloaded" });
  const gate = page.getByTestId("open-in-browser").first();
  await expect(gate.or(page.getByTestId("join-name-step"))).toBeVisible({ timeout: 15_000 });
  if (await gate.isVisible()) await gate.click();
  await page.getByTestId("join-name").fill("Ben");
  await page.getByTestId("join-password").fill("couch42");
  await page.getByTestId("join-go").click();
  // A first room asks to accept the rules once ("you're responsible for what you say and do").
  const accept = page.getByRole("button", { name: "I understand" });
  await expect(accept).toBeVisible({ timeout: 10_000 });
  await accept.click();
  if (await page.getByTestId("join-go").isVisible()) await page.getByTestId("join-go").click();
  await expect.poll(async () => (await state()).pending?.[0]?.name, { timeout: 30_000 }).toBe("Ben");
  const p = (await state()).pending![0];
  expect(p.password_ok).toBe(true);
  await api("/api/join/decide", { id: p.id, allow: true });
  await expect(page.getByTestId("join-player")).toHaveText("You are Player 2", { timeout: 30_000 });
  await expect
    .poll(async () => {
      const g = (await state()).guests?.[0];
      return g ? g.via : "";
    })
    .toBe("internet");
  // A wrong password never got this far: nothing in the hub's log names the room or the guest.
  expect(hubLog).not.toContain(card!.room);
  expect(hubLog).not.toContain("Couch chaos");
  await page.getByTestId("join-leave").click();
  await api("/api/join/close", { addr: consoleAddr });
});

test("an older hub: the room says it has no public rooms, the lobby waits quietly, internet invites work", async ({ page }) => {
  test.skip(!OLD_HUB, "a hub from before rooms (PS5UPLOAD_E2E_HUB_BIN + PS5UPLOAD_E2E_HUB_OLD)");
  test.setTimeout(240_000);
  const engineStart = engineLog.length;
  await openPublicRoom();
  await expect.poll(async () => (await state()).room?.listed?.why, { timeout: 30_000 }).toBe("old_hub");
  // The rooms list through the engine: the old hub's 404, which the apps read as "no public rooms".
  const r = await fetch(`${engine}/api/join/rooms`);
  expect(r.status).toBe(404);
  // The website's own fetch of an old hub: no CORS header, so the page says it can't reach the list.
  const list = await hubGet("/rooms", siteOrigin);
  expect(list.status).toBe(404);

  // An internet invite still works on the same hub, with the lobby on.
  const inv = (await api("/api/join/invite", { addr: consoleAddr, internet: true })) as { key: string; internet: { url: string } };
  await expect.poll(async () => (await state()).invite?.internet?.state, { timeout: 15_000 }).toBe("hosted");
  await page.goto(inv.internet.url.replace(SITE, `${siteOrigin}/join/`), { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("open-in-gate")).toBeVisible({ timeout: 15_000 });
  await page.getByTestId("open-in-browser").first().click();
  await page.getByTestId("join-name").fill("Ben");
  await page.getByTestId("join-go").click();
  await expect.poll(async () => (await state()).pending?.[0]?.name, { timeout: 30_000 }).toBe("Ben");
  await api("/api/join/decide", { id: (await state()).pending![0].id, allow: true });
  await expect(page.getByTestId("join-player")).toHaveText("You are Player 2", { timeout: 15_000 });

  // Quiet: the lobby tried the old hub once, gave the room it opened back at once (`leave`), and
  // waits 10 minutes before the next try; neither side logs a stream of errors.
  const m0 = await hubMetrics();
  await page.waitForTimeout(40_000);
  const m1 = await hubMetrics();
  // The hub's log: a start line and count summaries ("errors=0") at most.
  const hubErrors = hubLog.split("\n").filter((l) => /panic|warn|errors=[1-9]|refused=[1-9]/i.test(l));
  const engineErrors = engineLog
    .slice(engineStart)
    .split("\n")
    .filter((l) => /panic|rooms hub is unreachable|the hub says/.test(l));
  console.log(`old hub over 40 s: hosts ${m0.hosts}->${m1.hosts}, open rooms ${m1.rooms} (invite only), errors ${JSON.stringify(m1.errors)}; hub log:\n${hubLog}`);
  expect(m1.hosts - m0.hosts, "no new host socket while the lobby waits").toBe(0);
  expect(m1.rooms, "only the invite's room is open: the lobby's was given back").toBe(1);
  expect(m1.errors, "no protocol errors on the old hub").toEqual({});
  expect(hubErrors).toEqual([]);
  expect(engineErrors).toEqual([]);
  expect((await state()).room?.listed?.why).toBe("old_hub");
  // Closing Play Together ends the friend's game too (confirmed: a friend is still playing; no
  // Leave click, its page may have moved on to another screen in the 40 s).
  await api("/api/join/close", { addr: consoleAddr, confirm_friends: true });
});
