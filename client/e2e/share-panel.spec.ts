import { expect, test, type Locator, type Page } from "@playwright/test";
import { routeEngine } from "./fixtures/engine";

// Share, both sides, with a fake engine: the choices (homebrew app, save, screenshots, cheats,
// controls), the link with a friend asking and one receiving, and Receive's offer, progress and
// done. At desktop and phone width, light and dark, nothing is cut by the panel's edges and the
// primary button stays inside it. SHARE_SHOTS=<dir> also saves a screenshot of each state.

const ID = "FAKE77031";
const HOST = "192.168.1.100";
const LINK = "https://ps5upload.phantomptr.com/share/#room=AbCdEfGhIjKlMnOpQrStUv&fp=AA&hub=h";
const shots = process.env.SHARE_SHOTS ?? "";

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "12.1.0",
};

function made(requests: unknown[]) {
  return {
    id: "s1",
    state: "open",
    from: "Sam's MacBook",
    title: "Playden",
    title_id: ID,
    one_use: true,
    expires_in_s: 86_340,
    url: LINK,
    lan_url: null,
    works: "anywhere",
    items: [],
    requests,
  };
}

const items = [
  { id: "i1", kind: "app", name: "Playden", title_id: ID, format: "folder", size: 182_000_000, files: 40, refused: null },
  { id: "i2", kind: "save", name: "Playden save", title_id: ID, format: null, size: 1_200_000, files: 1, refused: null },
  { id: "i3", kind: "note", name: "Note", title_id: null, format: null, size: 40, files: 1, refused: null },
  {
    id: "i4",
    kind: "app",
    name: "ASTRO BOT",
    title_id: "PPSA21564",
    format: "folder",
    size: 9,
    files: 1,
    refused: "This is a retail game (PPSA21564). PS5Upload only shares homebrew.",
  },
];

function rx(state: string, extra: Record<string, unknown> = {}) {
  return {
    id: "r1",
    state,
    from: "Sam",
    title: "Playden",
    title_id: ID,
    items,
    accepted: [],
    expires_in_s: 3600,
    bytes_done: 0,
    bytes_total: 0,
    error: null,
    path: null,
    dest: null,
    received: [],
    ...extra,
  };
}

async function setup(page: Page, width: number, theme: string, appVerdict: "ok" | "retail" = "ok") {
  await page.setViewportSize({ width, height: 900 });
  await page.addInitScript(
    ([host, theme, id]) => {
      window.localStorage.setItem("ps5upload.host", host);
      window.localStorage.setItem("ps5upload.theme", theme);
      window.localStorage.setItem("ps5upload.rp_controls.v1", JSON.stringify({ [id]: {} }));
    },
    [HOST, theme, ID],
  );
  const state = { share: made([]) as ReturnType<typeof made>, rx: rx("offered") as Record<string, unknown> };
  await routeEngine(page, (path, req) => {
    const m = req.method();
    if (path === "/api/version") return { version: "12.1.0", caps: {} };
    if (path === "/api/ps5/status") return status;
    if (path === "/api/mesh/me") return { id: "d1", name: "Sam's MacBook", kind: "desktop", os: "macos", app: "12.1.0" };
    if (path === "/api/ps5/apps/installed") return { titles: [{ title_id: ID, title_name: "Playden", origin: "folder" }] };
    if (path === `/api/games/${ID}`)
      return {
        title_id: ID,
        title: "Playden",
        platform: "PS5",
        cover: null,
        copies: [],
        consoles: [
          { host: HOST, read_at: Math.floor(Date.now() / 1000), installed: true, version: "01.000", registered_from: `/user/app/${ID}`, dlc_missing: [] },
        ],
      };
    if (path === "/api/share/check")
      return {
        items: [
          appVerdict === "ok"
            ? { item: 0, ok: true }
            : { item: 0, ok: false, code: "retail", title_id: "PPSA21564", error: "This is a retail game (PPSA21564). PS5Upload only shares homebrew." },
        ],
      };
    if (path === "/api/ps5/saves/list")
      return {
        saves: [
          { title_id: ID, user_id: 1, path: `/user/home/1/savedata_prospero/${ID}`, size: 1_200_000, mtime: 1, kind: "ps5" },
          { title_id: ID, user_id: 2, path: `/user/home/2/savedata_prospero/${ID}`, size: 900_000, mtime: 1, kind: "ps5" },
        ],
      };
    if (path === "/api/ps5/screenshots/list")
      return {
        items: [1, 2, 3].map((n) => ({ path: `/user/av_contents/photo/${ID}/shot${n}.jxr`, size: 2_000_000, mtime: n })),
      };
    if (path === "/api/ps5/videos/list") return { items: [{ path: `/user/av_contents/video/${ID}/clip.webm`, size: 30_000_000, mtime: 1 }] };
    if (path === "/api/ps5/cheats/get")
      return { mods: [{ name: "Infinite HP", on: true }, { name: "Speed x2", on: false }, { name: "All items", on: true }] };
    if (path === "/api/share" && m === "POST") return state.share;
    if (path === "/api/share")
      return {
        shares: [],
        receives: [],
        history: [
          { at_ms: Date.UTC(2026, 10, 7, 12), dir: "sent", name: "Alex", items: ["Playden"], state: "done" },
          { at_ms: Date.UTC(2026, 10, 6, 12), dir: "received", name: "Bo", items: ["Note"], state: "declined" },
        ],
      };
    if (path === "/api/share/s1") return state.share;
    if (path === "/api/share/receive" && m === "POST") return { id: "r1" };
    if (path === "/api/share/receive/r1") return state.rx;
    return {};
  });
  return state;
}

/** Nothing sideways out of the panel, the page doesn't scroll sideways, and `button` sits
 *  inside the panel with room from its edges. */
async function fits(page: Page, panel: Locator, button: Locator) {
  const p = (await panel.boundingBox())!;
  const b = (await button.boundingBox())!;
  expect(b.x).toBeGreaterThanOrEqual(p.x + 8);
  expect(b.x + b.width).toBeLessThanOrEqual(p.x + p.width - 8);
  expect(b.y + b.height).toBeLessThanOrEqual(p.y + p.height);
  const overflow = await panel.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return [...el.querySelectorAll("p, button, input, textarea, h3, [role=radio]")].some((c) => {
      const q = c.getBoundingClientRect();
      return q.width > 0 && (q.left < r.left + 4 || q.right > r.right - 4);
    });
  });
  expect(overflow).toBe(false);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
}

async function shot(page: Page, name: string) {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png` });
}

for (const width of [1280, 390]) {
  for (const theme of ["light", "dark"]) {
    test(`Share: pick, link, requests (${width}px, ${theme})`, async ({ page }) => {
      const state = await setup(page, width, theme);
      await page.goto(`/games/${ID}`, { waitUntil: "domcontentloaded" });
      await page.getByRole("button", { name: "Share", exact: true }).first().click({ timeout: 60_000 });
      const dialog = page.getByRole("dialog");
      await expect(page.getByTestId("share-panel")).toBeVisible();
      await expect(page.getByTestId("share-title")).toHaveText("Playden");

      // The rows that exist for this game, the app ready once the engine said so.
      await expect(page.getByTestId("share-app")).toHaveAttribute("data-state", "on");
      await expect(page.getByTestId("share-media")).toContainText("3 screenshots, 1 clip · 34.3 MiB");
      await expect(page.getByTestId("share-cheats")).toContainText("3 cheats · 2 on now");
      await expect(page.getByTestId("share-controls")).toBeVisible();
      await expect(page.getByText("aren't kept per game")).toHaveCount(0);
      await page.getByTestId("share-save").getByRole("checkbox").click();
      await expect(page.getByTestId("share-save-slot")).toHaveCount(2);
      await page.getByTestId("share-media").getByRole("checkbox").click();
      // Name from the device, a lifetime, who can use it.
      await expect(page.getByTestId("share-name-shown")).toHaveText("Sam's MacBook");
      await page.getByRole("radio", { name: "Anyone with the link" }).click();
      await expect(page.getByTestId("share-who-hint")).toContainText("allow each one");
      await page.getByRole("radio", { name: "One friend" }).click();
      await expect(page.getByTestId("share-who-hint")).toContainText("after you allow the first person");
      await page.getByTestId("share-note").fill("Try the second level!");
      await page.waitForTimeout(300);
      await fits(page, dialog, page.getByTestId("share-go"));
      await shot(page, `pick-bottom-${width}-${theme}`);
      await page.getByTestId("share-panel").evaluate((el) => el.parentElement?.scrollTo(0, 0));
      await page.waitForTimeout(200);
      await shot(page, `pick-${width}-${theme}`);

      // Share: the same panel shows the link, then a friend asks and another receives.
      state.share = made([
        { request: 1, name: "Alex", wants: ["i1", "i2"], wants_names: ["Playden", "Playden save"], state: "asking", bytes_done: 0, bytes_total: 0, why: null },
        { request: 2, name: "Bo", wants: ["i1"], wants_names: ["Playden"], state: "sending", bytes_done: 61_000_000, bytes_total: 182_000_000, why: null },
      ]);
      await page.getByTestId("share-go").click();
      await expect(page.getByTestId("share-qr")).toBeVisible();
      await expect(page.getByTestId("share-url")).toHaveText(LINK);
      await expect(page.getByTestId("share-expires")).toContainText("Expires in 23 h");
      await expect(page.getByTestId("share-request")).toHaveCount(2, { timeout: 10_000 });
      await expect(page.getByTestId("share-allow")).toBeVisible();
      await page.waitForTimeout(300);
      await fits(page, dialog, page.getByTestId("share-done"));
      await shot(page, `made-${width}-${theme}`);
      await page.getByTestId("share-done").click();
      await expect(page.getByTestId("share-panel")).toHaveCount(0);
    });

    test(`Share: a retail app is a quiet row (${width}px, ${theme})`, async ({ page }) => {
      await setup(page, width, theme, "retail");
      await page.goto(`/games/${ID}`, { waitUntil: "domcontentloaded" });
      await page.getByRole("button", { name: "Share", exact: true }).first().click({ timeout: 60_000 });
      const app = page.getByTestId("share-app");
      await expect(app).toHaveAttribute("data-state", "unavailable");
      await expect(page.getByTestId("share-app-reason")).toHaveText("Retail games can't be shared");
      await page.getByTestId("share-app-why").hover();
      await expect(app.getByRole("tooltip")).toContainText("This is a retail game (PPSA21564)");
      await shot(page, `pick-retail-${width}-${theme}`);
    });

    test(`Receive: offer, progress, done (${width}px, ${theme})`, async ({ page }) => {
      const state = await setup(page, width, theme);
      await page.goto(`/share?receive=${encodeURIComponent(LINK)}`, { waitUntil: "domcontentloaded" });
      await expect(page.getByTestId("receive-link")).toHaveValue(LINK, { timeout: 60_000 });
      await expect(page.getByTestId("receive-name-shown")).toHaveText("Sam's MacBook");
      await shot(page, `receive-open-${width}-${theme}`);
      await page.getByTestId("receive-open").click();
      const offer = page.getByTestId("receive-offer");
      await expect(offer).toBeVisible();
      await expect(offer).toContainText("Sam shared Playden");
      const rows = page.getByTestId("receive-item");
      await expect(rows).toHaveCount(4);
      await expect(rows.nth(3)).toHaveAttribute("data-state", "unavailable");
      await expect(rows.nth(0)).toHaveAttribute("data-state", "on");
      await rows.nth(2).getByRole("checkbox").click();
      await expect(rows.nth(2)).toHaveAttribute("data-state", "off");
      await fits(page, offer, page.getByTestId("receive-accept"));
      await shot(page, `receive-offer-${width}-${theme}`);

      state.rx = rx("receiving", { accepted: ["i1", "i2"], bytes_done: 92_000_000, bytes_total: 183_200_000, path: "internet" });
      await page.getByTestId("receive-accept").click();
      await expect(page.getByTestId("receive-progress-text")).toContainText("Receiving", { timeout: 10_000 });
      await fits(page, page.getByTestId("receive-progress"), page.getByTestId("receive-cancel"));
      await shot(page, `receive-progress-${width}-${theme}`);

      state.rx = rx("done", {
        accepted: ["i1", "i2"],
        dest: "/Users/sam/PS5Upload/Received/Playden from Sam",
        received: [
          { item: "i1", kind: "app", path: "/Users/sam/PS5Upload/Received/Playden from Sam/Playden" },
          { item: "i2", kind: "save", path: "/Users/sam/PS5Upload/Received/Playden from Sam/Playden save" },
        ],
      });
      await expect(page.getByTestId("receive-done")).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId("receive-dest")).toContainText("Received/Playden from Sam");
      await fits(page, page.getByTestId("receive-done"), page.getByTestId("receive-another"));
      await shot(page, `receive-done-${width}-${theme}`);
    });
  }
}

// Receive moved from Play to its own Share screen (12.3): an old /play?receive= link lands there,
// with the link filled in, and Play has no Receive tab any more.
test("an old Play › Receive link opens the Share screen", async ({ page }) => {
  await setup(page, 1280, "light");
  await page.goto(`/play?receive=${encodeURIComponent(LINK)}`, { waitUntil: "domcontentloaded" });
  await expect(page).toHaveURL(/\/share$/, { timeout: 60_000 });
  await expect(page.getByTestId("share-screen")).toBeVisible();
  await expect(page.getByTestId("receive-link")).toHaveValue(LINK);
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("radio", { name: "Play Together" })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("radio", { name: "Receive" })).toHaveCount(0);
});

// A second link handed over while the Share screen already shows (an app link opened again) goes
// into the form; it used to be taken off the address and lost.
test("a second link opened on the Share screen fills the form", async ({ page }) => {
  await setup(page, 1280, "light");
  await page.goto(`/share?receive=${encodeURIComponent(LINK)}`, { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("receive-link")).toHaveValue(LINK, { timeout: 60_000 });
  await expect(page).toHaveURL(/\/share$/);
  const second = `${LINK}-2`;
  await page.evaluate((url) => {
    history.pushState(null, "", url);
    dispatchEvent(new PopStateEvent("popstate"));
  }, `/share?receive=${encodeURIComponent(second)}`);
  await expect(page.getByTestId("receive-link")).toHaveValue(second);
  await expect(page).toHaveURL(/\/share$/);
});
