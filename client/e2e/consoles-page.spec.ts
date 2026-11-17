import { expect, test, type Page, type Route } from "@playwright/test";

// The Consoles page (Connection's console tab) against a mocked engine: no consoles (the Add
// sheet opens), one ready, one needing the helper, one away on Tailscale, the Add sheet with what
// the home and Tailscale scans found merged into one row, an invite's QR code, and a device asking
// to connect (Allow / Decline over every screen). Each state at 1280 px and 390 px, light and dark,
// with no sideways scroll. CONSOLES_SHOTS_DIR=<dir> also keeps a screenshot of each.

const SHOTS = process.env.CONSOLES_SHOTS_DIR;
const HOST = "192.168.1.100";
const TS_IP = "100.101.4.7";
const KEY = "ab".repeat(32);
const ASKER = "cd".repeat(32);
const LIVING = { id: "p1", name: "Living room", host: HOST };

type State = "none" | "ready" | "helper" | "away" | "asking";

const STATUS_UP = {
  version: "12.1.0",
  ps5_kernel: "FreeBSD 11.0 releng/11.0 #0 r218215/releases/09.60: Mon Aug 26 2024",
  ucred_elevated: true,
};

function mesh(asking: boolean) {
  return {
    supported: true,
    rev: asking ? 2 : 1,
    now: Date.now(),
    you: "me".padEnd(64, "0"),
    helper: "12.1.0",
    settings: { require_approval: true, prune_days: 90 },
    asks: asking ? [{ id: ASKER, name: "Pixel 9", ip: "192.168.1.30", at: Date.now() - 4000 }] : [],
    devices: [
      { id: "me".padEnd(64, "0"), name: "MacBook", nick: "", kind: "desktop", os: "macOS", app: "12.1.0", first: 1, last: Date.now(), ip: "192.168.1.20", online: true, blocked: false, activity: [], via: "" },
      { id: "11".repeat(32), name: "Pixel 8", nick: "", kind: "android", os: "Android 16", app: "12.1.0", first: 1, last: Date.now() - 3600_000, ip: "192.168.1.31", online: false, blocked: false, activity: [], via: "approved" },
      { id: "22".repeat(32), name: "Work laptop", nick: "", kind: "desktop", os: "Windows", app: "12.1.0", first: 1, last: Date.now() - 86400_000, ip: "100.88.2.3", online: false, blocked: false, activity: [], via: "tailscale" },
    ],
    locks: [],
    events: [],
    ev: 1,
  };
}

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });

async function setUp(page: Page, state: State, theme: "light" | "dark", onDevice?: (body: unknown) => void) {
  await page.addInitScript(
    ([profile, withConsole, th]) => {
      window.localStorage.setItem("ps5upload.theme", th as string);
      if (!withConsole) return;
      const p = profile as { id: string; host: string };
      window.localStorage.setItem("ps5upload.host", p.host);
      window.localStorage.setItem("ps5upload.roster.v1", JSON.stringify({ active_id: p.id, profiles: [p] }));
    },
    [LIVING, state !== "none", theme] as const,
  );
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const u = new URL(route.request().url());
      const path = u.pathname;
      if (path === "/api/ps5/status") {
        if (state === "helper")
          return json(route, { error: "the helper is not running", reason: "ava1_unreachable", power: "awake" }, 502);
        return json(route, STATUS_UP);
      }
      if (path === "/api/mesh/state") return json(route, mesh(state === "asking"));
      if (path === "/api/mesh/me") return json(route, { id: "me", name: "MacBook", kind: "desktop", os: "macOS", app: "12.1.0" });
      if (path === "/api/tailnet")
        return json(route, {
          ok: true,
          enabled: true,
          state: "running",
          backend: "builtin",
          signed_in: true,
          consoles:
            state === "away" || state === "ready" || state === "asking"
              ? [{ host: HOST, ip: TS_IP, name: "living-room", via: state === "away" ? "relayed" : "lan", latency_ms: 80, tested_at: Date.now() - 86400_000 }]
              : [],
        });
      if (path === "/api/ps5/tailnet")
        return json(route, {
          ok: true,
          enabled: true,
          signed_in: true,
          state: "running",
          ip: TS_IP,
          name: "living-room-ps5u.tail1234.ts.net.",
          hostname: "living-room-ps5u",
          allow_mode: "listed",
          allow: "100.88.2.3,100.64.0.9",
          peers: [],
          blocked_recent: [],
        });
      if (path === "/api/mesh/device") {
        onDevice?.(route.request().postDataJSON());
        return json(route, { supported: true, ok: true, rev: 3 });
      }
      if (path === "/api/ava1/console") return json(route, { key: KEY, name: "PS5" });
      if (path === "/api/tailnet/consoles/add") {
        onDevice?.({ add: route.request().postDataJSON() });
        return json(route, { action: "added", console: { id: "p2", name: "office", host: "192.168.1.120" }, found: {} });
      }
      if (path === "/api/discover")
        return json(route, {
          ok: true,
          scanned_ms: 2900,
          browsed_services: [],
          candidates: [
            { ip: "192.168.1.120", hostname: "PS5-Office", all_ips: ["192.168.1.120"], services: [], loader_port_open: true, payload_port_open: true, confidence: 90, likely_ps5: true },
            { ip: "192.168.1.77", hostname: null, all_ips: ["192.168.1.77"], services: [], loader_port_open: true, payload_port_open: false, confidence: 30, likely_ps5: false },
          ],
        });
      if (path === "/api/tailnet/consoles")
        return json(route, {
          backend: "builtin",
          can_list: true,
          consoles: [
            { name: "office", tailnet_name: "office-ps5u", tailnet_ip: "100.101.9.9", lan_host: "192.168.1.120", id: KEY, reachable: true, via: "builtin", online: true, paired: false, hosts: [], listed: null, error: null },
          ],
        });
      return json(route, {});
    },
  );
  await page.goto("/connection", { waitUntil: "domcontentloaded" });
}

const main = (page: Page) => page.locator("[data-scroll-root]:visible");

async function noSidewaysScroll(page: Page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  const inMain = await main(page).evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(inMain).toBeLessThanOrEqual(0);
}

async function shot(page: Page, name: string) {
  if (!SHOTS) return;
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${SHOTS}/${name}.png`, fullPage: false });
}

for (const width of [1280, 390]) {
  for (const theme of ["light", "dark"] as const) {
    const tag = `${width}-${theme}`;
    test.describe(`Consoles at ${tag}`, () => {
      test.beforeEach(async ({ page }) => {
        await page.setViewportSize({ width, height: width > 600 ? 900 : 844 });
      });

      test("no consoles: a friendly empty state with the Add sheet open, scan first", async ({ page }) => {
        await setUp(page, "none", theme);
        const sheet = page.getByRole("dialog", { name: "Add a PS5" });
        await expect(sheet).toBeVisible({ timeout: 30_000 });
        await expect(sheet.getByText("Let's find your PS5.")).toBeVisible();
        await expect(sheet.getByTestId("consoles-found-row").first()).toBeVisible();
        await shot(page, `01-no-consoles-sheet-${tag}`);
        await sheet.getByRole("button", { name: "Close" }).first().click();
        await expect(main(page).getByTestId("consoles-empty")).toBeVisible();
        await noSidewaysScroll(page);
        await shot(page, `01-no-consoles-${tag}`);
      });

      test("one console, ready: status at a glance, no checklist", async ({ page }) => {
        await setUp(page, "ready", theme);
        const card = main(page).getByTestId("console-card");
        await expect(card).toHaveAttribute("data-ready", "1", { timeout: 30_000 });
        await expect(card.getByTestId("console-card-status")).toHaveText(`Online · Home network ${HOST}`);
        await expect(card.getByText("Helper v12.1.0 · matches this app")).toBeVisible();
        await expect(card.getByTestId("consoles-checklist")).toHaveCount(0);
        await expect(card.getByTestId("consoles-ts-tested")).toBeVisible();
        await card.getByTestId("consoles-devices-toggle").click();
        await expect(card.getByTestId("consoles-trusted-device")).toHaveCount(3);
        await noSidewaysScroll(page);
        await shot(page, `02-ready-${tag}`);
        await card.getByTestId("consoles-trusted").scrollIntoViewIfNeeded();
        await shot(page, `02-ready-devices-${tag}`);
      });

      test("one console needing the helper: the checklist is in the card", async ({ page }) => {
        await setUp(page, "helper", theme);
        const card = main(page).getByTestId("console-card");
        await expect(card.getByTestId("consoles-checklist")).toBeVisible({ timeout: 30_000 });
        await expect(card.getByTestId("consoles-step-helper")).toHaveAttribute("data-mark", "now");
        await expect(card.getByTestId("consoles-elfldr")).toBeVisible();
        await expect(card.getByText("Invite a device")).toHaveCount(0);
        await noSidewaysScroll(page);
        await shot(page, `03-needs-helper-${tag}`);
      });

      test("one console away on Tailscale", async ({ page }) => {
        await setUp(page, "away", theme);
        const card = main(page).getByTestId("console-card");
        await expect(card.getByTestId("console-card-status")).toHaveText(`Online · Tailscale ${TS_IP}, relayed`, { timeout: 30_000 });
        await expect(card.getByTestId("consoles-reach-tailscale").getByText("In use")).toBeVisible();
        await expect(card.getByText("2 devices you ticked")).toBeVisible();
        await noSidewaysScroll(page);
        await card.getByTestId("consoles-reach-tailscale").scrollIntoViewIfNeeded();
        await shot(page, `04-away-tailscale-${tag}`);
      });

      test("the Add sheet merges what the scans found: one row per console", async ({ page }) => {
        await setUp(page, "ready", theme);
        await main(page).getByTestId("consoles-add-open").click({ timeout: 30_000 });
        const sheet = page.getByRole("dialog", { name: "Add a PS5" });
        // The office PS5 found at home and on Tailscale is one row with both badges.
        const rows = sheet.getByTestId("consoles-found-row");
        await expect(rows).toHaveCount(2, { timeout: 30_000 });
        const office = rows.filter({ hasText: "office" });
        await expect(office.getByText("Home network")).toBeVisible();
        await expect(office.getByText("Tailscale")).toBeVisible();
        await shot(page, `05-add-merged-${tag}`);
        // Pasting an invite for the same console joins that row too.
        await sheet
          .getByTestId("consoles-invite-paste")
          .fill(`ps5upload://console?n=Office&h=192.168.1.120&t=100.101.9.9&k=${KEY}`);
        await expect(sheet.getByTestId("consoles-invite-read")).toBeVisible();
        await expect(rows).toHaveCount(2);
        await expect(rows.filter({ hasText: "Office" }).getByText("Invite")).toBeVisible();
        await noSidewaysScroll(page);
        await shot(page, `05-add-merged-invite-${tag}`);
      });

      test("Invite a device: the QR code, and the console's key only inside the link", async ({ page }) => {
        await setUp(page, "ready", theme);
        await main(page).getByTestId("consoles-invite-open").click({ timeout: 30_000 });
        const sheet = page.getByRole("dialog", { name: "Invite a device to Living room" });
        await expect(sheet.getByTestId("consoles-invite-qr")).toBeVisible();
        await expect(sheet.getByTestId("consoles-invite-key")).toHaveCount(0);
        await expect(sheet.getByText("abab abab")).toHaveCount(0);
        await expect(sheet.getByTestId("consoles-invite-link")).toHaveValue(new RegExp(`^ps5upload://console\\?n=Living\\+room&h=${HOST}&t=${TS_IP}&k=${KEY}$`));
        await noSidewaysScroll(page);
        await shot(page, `06-invite-${tag}`);
      });

      test("a PS5 on Tailscale that isn't on the list: offered on the page, with Add", async ({ page }) => {
        const sent: unknown[] = [];
        await setUp(page, "ready", theme, (b) => sent.push(b));
        const found = main(page).getByTestId("consoles-ts-found");
        await expect(found).toBeVisible({ timeout: 30_000 });
        await expect(found.getByTestId("consoles-ts-found-row")).toHaveCount(1);
        await expect(found.getByTestId("consoles-ts-found-row")).toContainText("office");
        await noSidewaysScroll(page);
        await shot(page, `08-ts-found-${tag}`);
        await found.getByTestId("consoles-ts-found-add").click();
        await expect.poll(() => sent).toContainEqual({ add: { addr: "100.101.9.9", name: "office" } });
      });

      test("a device asking: Allow on the card (once, no prompt over it), and Allow reaches the console", async ({ page }) => {
        const answers: unknown[] = [];
        await setUp(page, "asking", theme, (b) => answers.push(b));
        const ask = main(page).getByTestId("consoles-ask");
        await expect(ask).toBeVisible({ timeout: 30_000 });
        await expect(ask).toHaveCount(1);
        // The card answers its own asks here: the floating prompt stands aside on this page.
        await expect(page.getByTestId("console-ask-prompt")).toHaveCount(0);
        await noSidewaysScroll(page);
        await shot(page, `07-approval-${tag}`);
        await ask.getByTestId("consoles-ask-allow").click();
        await expect.poll(() => answers).toContainEqual({ addr: HOST, op: "allow", id: ASKER });
      });
    });
  }
}
