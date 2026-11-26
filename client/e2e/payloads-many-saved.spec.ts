import { expect, test, type Page } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// "On your PS5" for a console with a long Payload Manager list: 20 saved payloads, four more
// found on the console (Payload Manager itself, and the settings ShadowMount+, nanoDNS and Hush
// left there) and elfldr running. The 20 are the tab's main list, with a box to narrow them; the number at the top
// is what the three sections add up to; a data folder left behind (LZ4 Compression's) is no card.
//
// PS5UPLOAD_SHOTS=<dir> also saves screenshots (desktop, tablet, phone; light, German, Russian,
// Arabic).

const SHOTS = process.env.PS5UPLOAD_SHOTS;
// A wide font, as Linux CI has (the label specs do the same).
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

const proc = (pid: number, comm: string, extra: Record<string, unknown> = {}) => ({
  pid,
  name: comm,
  comm,
  title_id: "",
  app_id: 0,
  memory_mib: 3.2,
  threads: 4,
  kind: "payload",
  ...extra,
});

const PROCESSES = [
  proc(97, "pldmgr.elf"),
  proc(102, "kstuff.elf"),
  proc(104, "ps5upload.elf", { is_self: true }),
  proc(106, "elfldr.elf"),
  proc(60, "SceShellUI", { kind: "system" }),
];

const LONG = "a-payload-with-a-very-long-file-name-that-keeps-going-without-any-space-1.0.0-beta-rc2";
const STEMS = [
  "kstuff-1.13-fpkg",
  "kraken-mount",
  "syslang-zh-Hans-0.3",
  "AnyPad-PS5-0.9.1-beta",
  "ftpsrv",
  "websrv",
  "klogsrv",
  "gdbsrv",
  "shsrv",
  "etaHEN-2.4b",
  "ps5debug",
  "fan-control",
  "np-fake-signin",
  "backpork",
  "airpsx",
  "app-dumper",
  "itemzflow",
  "game-patch",
  "ps5-versions",
  LONG,
];

const PM_ITEMS = STEMS.map((stem) => ({
  slot: `local/${stem}`,
  path: `/data/pldmgr/payloads/local/${stem}/${stem}.elf`,
  kind: "local",
  filename: `${stem}.elf`,
  folder: `local/${stem}`,
  name: "",
  version: "",
  source: "",
  install_source: "local",
}));

const catalogEntry = (id: string, display: string, hint: string | null, marker: string | null, ports: number[] = []) => ({
  id,
  display_name: display,
  role: "",
  description: "",
  repo_owner: "o",
  repo_name: id,
  on_console_marker_path: marker,
  process_name_hint: hint,
  ports,
  autoload_priority: 0,
  autoload_delay_ms: 0,
  homepage: "",
});
const CATALOG = [
  catalogEntry("kstuff-echostretch", "kstuff-lite (EchoStretch)", null, "/data/kstuff.elf"),
  catalogEntry("payload-manager", "Payload Manager", null, "/data/pldmgr/pldmgr_config.txt", [8084]),
  catalogEntry("shadowmountplus", "ShadowMount+", "shadowmountplus", "/data/shadowmount/debug.log"),
  catalogEntry("nanodns", "nanoDNS", "nanodns.elf", "/data/nanodns/nanodns.ini", [53]),
  catalogEntry("hush", "Hush", "hush", "/data/hush/config.ini"),
  // Removed from this console; its data folder is still there.
  catalogEntry("lz4-compression", "LZ4 Compression (theghostonline)", null, "/data/LZ4Compression", [5920]),
];

const DIRS: Record<string, Array<{ name: string; kind: string }>> = {
  "/data": [
    { name: "pldmgr", kind: "dir" },
    { name: "shadowmount", kind: "dir" },
    { name: "nanodns", kind: "dir" },
    { name: "hush", kind: "dir" },
    { name: "LZ4Compression", kind: "dir" },
    { name: "kstuff.elf", kind: "file" },
  ],
  "/data/pldmgr": [{ name: "pldmgr_config.txt", kind: "file" }],
  "/data/shadowmount": [{ name: "debug.log", kind: "file" }],
  "/data/nanodns": [{ name: "nanodns.ini", kind: "file" }],
  "/data/hush": [{ name: "config.ini", kind: "file" }],
};

async function open(page: Page, opts: { lang?: string; theme?: string } = {}) {
  await seedConsole(page);
  await page.addInitScript(
    ({ lang, theme }) => {
      if (lang) window.localStorage.setItem("ps5upload.lang", lang);
      if (theme) window.localStorage.setItem("ps5upload.theme", theme);
    },
    { lang: opts.lang ?? null, theme: opts.theme ?? null },
  );
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status") return { ok: true };
    if (path === "/api/ps5/process/list") return { processes: PROCESSES, truncated: false };
    if (path === "/api/payloads") return { payloads: CATALOG, local: [] };
    if (path === "/api/ps5/pldmgr")
      return {
        present: true,
        version: "0.6.1",
        payloads: PM_ITEMS,
        autoload: { enabled: true, delay_s: 5, list: ["local/kstuff-1.13-fpkg", "!2000", "local/ftpsrv"] },
      };
    if (path === "/api/ps5/smp/status")
      return { installed: false, running: false, mounted_images: [], errors: [], mounter: "none", kraken: null };
    if (path === "/api/ps5/list-dir")
      return { entries: DIRS[new URL(req.url()).searchParams.get("path") ?? ""] ?? [], truncated: false };
    return undefined;
  });
  await page.goto("/payloads?tab=ps5", { waitUntil: "domcontentloaded" });
  const root = page.locator("[data-scroll-root]:visible");
  await expect(root.getByTestId("pm-saved-list").locator("li")).toHaveCount(20, { timeout: 60_000 });
  await expect(root.getByTestId("hub-also-running")).toBeVisible();
  return root;
}

/** Nothing in the tab is wider than the page, and no row's text runs out of its row. */
async function expectNoOverflow(page: Page, root: ReturnType<Page["locator"]>, what: string) {
  const sideways = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(sideways, `${what}: sideways scroll`).toBeLessThanOrEqual(0);
  const out = await root.locator("[data-testid='pm-saved-list'] li, [data-card]").evaluateAll((rows) =>
    rows
      .filter((row) => {
        const r = row.getBoundingClientRect();
        return [...row.querySelectorAll("span, div, button")].some((el) => {
          const b = el.getBoundingClientRect();
          return b.width > 0 && (b.left < r.left - 1 || b.right > r.right + 1);
        });
      })
      .map((row) => row.textContent?.slice(0, 40)),
  );
  expect(out, `${what}: text outside its row`).toEqual([]);
}

test.describe("desktop", () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test("20 saved payloads are the main list, and the count is what the sections add up to", async ({ page }) => {
    const root = await open(page);

    // 20 saved + 4 found + 1 also running.
    await expect(root.getByTestId("hub-count")).toHaveText("25 on this PS5");
    const saved = root.getByTestId("pm-saved-list");
    await expect(saved.getByRole("heading", { name: "Your payloads · 20" })).toBeVisible();
    const found = root.getByTestId("hub-detected");
    await expect(found.getByRole("heading")).toHaveText("Found on this PS5 · 4");
    await expect(found.locator("[data-card]")).toHaveCount(4);
    const also = root.getByTestId("hub-also-running");
    await expect(also.getByRole("heading")).toHaveText("Also running · 1");
    await expect(also.locator("[data-card]")).toHaveCount(1);

    // In order: your payloads, what was found, what else runs.
    const tops = await Promise.all([saved, found, also].map(async (l) => (await l.boundingBox())!.y));
    expect(tops[0]).toBeLessThan(tops[1]);
    expect(tops[1]).toBeLessThan(tops[2]);

    // Each row has its actions; the one that runs has Stop.
    const first = saved.locator("li").first();
    await expect(first).toContainText("kstuff-1.13-fpkg.elf");
    await expect(first).toContainText("Running · PID 102");
    for (const name of ["Launch", "Don't load at boot", "Stop", "Delete"]) {
      await expect(first.getByRole("button", { name, exact: true })).toBeVisible();
    }
    await expect(saved.locator("li").nth(1).getByRole("button", { name: "Load at boot" })).toBeVisible();

    // Payload Manager's own settings stay behind Manage, without a second copy of the list.
    const pmCard = root.locator('[data-card="payload-manager"]');
    await expect(pmCard.getByText("Load payloads at boot")).toHaveCount(0);
    await pmCard.getByRole("button", { name: "Manage" }).click();
    await expect(pmCard.getByText("Load payloads at boot")).toBeVisible();
    await expect(pmCard.getByText("Load the PS5Upload helper at boot")).toBeVisible();
    await expect(pmCard.getByText("kraken-mount.elf")).toHaveCount(0);
    await pmCard.getByRole("button", { name: "Manage" }).click();

    // A data folder left behind is not a payload: no card, nothing "Installed" for LZ4 Compression.
    await expect(root.locator('[data-card="lz4-compression"]')).toHaveCount(0);
    await expect(root.getByText("LZ4 Compression")).toHaveCount(0);
    // kstuff runs and is saved: it is on its row, not a second time as a card. Settings alone
    // (nanoDNS's .ini) keep a card for its Manage, and never read "Installed".
    await expect(root.locator('[data-card="kstuff-echostretch"]')).toHaveCount(0);
    await expect(root.getByText("Installed", { exact: true })).toHaveCount(0);
    await expect(root.locator('[data-card="nanodns"]')).toContainText("Not running");
    await expect(root.locator('[data-card="nanodns"]')).not.toContainText("Installed");

    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-1280-dark.png`, fullPage: false });
  });

  // A row that runs and is in the boot list has more buttons: its name and path still get the
  // width every other row's get (they share one column for their actions), so its path is cut at
  // the same place.
  test("every row's text column is as wide as the others, the running one too", async ({ page }) => {
    const root = await open(page);
    await page.addStyleTag({ content: WIDE_FONT });
    const saved = root.getByTestId("pm-saved-list");
    const widths = await saved.getByTestId("pm-row-text").evaluateAll((els) =>
      els.map((e) => Math.round(e.getBoundingClientRect().width)),
    );
    expect(widths).toHaveLength(20);
    expect(Math.max(...widths) - Math.min(...widths)).toBeLessThanOrEqual(1);
    // The actions end at the same place in every row.
    const ends = await saved.getByTestId("pm-row-actions").evaluateAll((els) =>
      els.map((e) => Math.round(e.getBoundingClientRect().right)),
    );
    expect(Math.max(...ends) - Math.min(...ends)).toBeLessThanOrEqual(1);
    // The running row keeps every button.
    const first = saved.locator("li").first();
    for (const name of ["Launch", "Don't load at boot", "Stop", "Delete"]) {
      await expect(first.getByRole("button", { name, exact: true })).toBeVisible();
    }
    await expectNoOverflow(page, root, "1280 wide font");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-1280-aligned.png` });
  });

  test("the filter box narrows a long list, by keyboard too", async ({ page }) => {
    const root = await open(page);
    const saved = root.getByTestId("pm-saved-list");
    const box = saved.getByRole("searchbox", { name: "Search payloads" });
    await expect(box).toBeVisible();

    await box.focus();
    await page.keyboard.type("SRV");
    await expect(saved.locator("li")).toHaveCount(5);
    await expect(saved.getByTestId("pm-filter-count")).toHaveText("5 of 20");
    // The heading and the tab's number still count every payload.
    await expect(saved.getByRole("heading", { name: "Your payloads · 20" })).toBeVisible();
    await expect(root.getByTestId("hub-count")).toHaveText("25 on this PS5");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-1280-filter.png` });

    // From the box, Tab reaches the first row's buttons.
    await page.keyboard.press("Tab");
    await expect(saved.locator("li").first().getByRole("button").first()).toBeFocused();

    await box.fill("no such payload");
    await expect(saved.locator("li")).toHaveCount(0);
    await expect(saved.getByText('No payloads match "no such payload".')).toBeVisible();
    await box.focus();
    await page.keyboard.press("Escape");
    await expect(box).toHaveValue("");
    await expect(saved.locator("li")).toHaveCount(20);
  });

  test("light theme, German, Russian and Arabic fit", async ({ page }) => {
    for (const [lang, theme] of [
      ["en", "light"],
      ["de", "dark"],
      ["ru", "dark"],
      ["ar", "dark"],
    ]) {
      const root = await open(page, { lang, theme });
      await expectNoOverflow(page, root, `${lang} 1280`);
      if (lang === "ar") {
        // A path reads left to right in a right-to-left page.
        const path = root.getByTestId("pm-saved-list").locator("li").first().locator(".font-mono");
        await expect(path).toHaveText("/data/pldmgr/payloads/local/kstuff-1.13-fpkg/kstuff-1.13-fpkg.elf");
        expect(await path.evaluate((el) => getComputedStyle(el).unicodeBidi)).toBe("plaintext");
      }
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-1280-${lang}-${theme}.png` });
    }
  });
});

test.describe("tablet", () => {
  test.use({ viewport: { width: 820, height: 1180 }, hasTouch: true });

  test("the list fits", async ({ page }) => {
    const root = await open(page);
    await expect(root.getByTestId("hub-count")).toHaveText("25 on this PS5");
    await expectNoOverflow(page, root, "tablet");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-820.png` });
  });
});

test.describe("phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test("20 saved payloads on a phone: all listed, nothing cut, the filter works", async ({ page }) => {
    const root = await open(page);
    await expect(root.getByTestId("hub-count")).toHaveText("25 on this PS5");
    const saved = root.getByTestId("pm-saved-list");
    await expectNoOverflow(page, root, "phone");
    // The long name is whole, on as many lines as it takes.
    const long = saved.locator("li").last();
    await expect(long).toContainText(`${LONG}.elf`);
    const name = long.getByTestId("pm-row-name");
    expect(await name.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) {
      await page.screenshot({ path: `${SHOTS}/many-390.png` });
      await long.scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}/many-390-long-name.png` });
      await root.evaluate((el) => el.scrollTo(0, 0));
    }

    await saved.getByRole("searchbox", { name: "Search payloads" }).fill("kraken");
    await expect(saved.locator("li")).toHaveCount(1);
    await expect(saved.getByTestId("pm-filter-count")).toHaveText("1 of 20");
    await expect(root.getByTestId("hub-detected").locator("[data-card]")).toHaveCount(4);
    await expect(root.getByTestId("hub-also-running").locator("[data-card]")).toHaveCount(1);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-390-filter.png` });
  });

  // A compact row: the name and its state on one line, the main action as one button, the rest
  // behind ⋯. Before, each row's buttons took two or three lines.
  test("a saved payload is one compact row: name, state, one button, ⋯", async ({ page }) => {
    const root = await open(page);
    await page.addStyleTag({ content: WIDE_FONT });
    const saved = root.getByTestId("pm-saved-list");
    const rows = saved.locator("li");
    await expect(rows.first()).toHaveAttribute("data-compact", "");
    // The running one, first at boot: its state, Stop, and ⋯ with the rest.
    const first = rows.first();
    await expect(first).toContainText("kstuff-1.13-fpkg.elf");
    await expect(first).toContainText("Running");
    await expect(first).toContainText("boot #1");
    await expect(first.getByRole("button", { name: "Stop", exact: true })).toBeVisible();
    await expect(first.getByRole("button", { name: "Launch", exact: true })).toHaveCount(0);
    await expect(first.getByRole("button", { name: "Delete", exact: true })).toHaveCount(0);
    // A plain one: Launch.
    await expect(rows.nth(1).getByRole("button", { name: "Launch", exact: true })).toBeVisible();
    // As tall as the 44 px button and the row's padding: a name that wraps to two lines fits
    // beside it (before, a row was about 170 px).
    const heights = await rows.evaluateAll((els) =>
      els.slice(0, 19).map((e) => Math.round(e.getBoundingClientRect().height)),
    );
    expect(Math.max(...heights)).toBeLessThanOrEqual(64);
    // ⋯ holds the rest.
    await first.getByRole("button", { name: "More for kstuff-1.13-fpkg.elf" }).click();
    const menu = page.getByRole("menu");
    for (const name of ["Don't load at boot", "Load earlier", "Load later", "Launch", "Delete"]) {
      await expect(menu.getByRole("menuitem", { name, exact: true })).toBeVisible();
    }
    await expect(menu.getByRole("menuitem", { name: "Load earlier", exact: true })).toBeDisabled();
    const m = (await menu.boundingBox())!;
    expect(m.x).toBeGreaterThanOrEqual(0);
    expect(m.x + m.width).toBeLessThanOrEqual(390.5);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-390-compact-menu.png` });
    await page.keyboard.press("Escape");
    await expectNoOverflow(page, root, "phone wide font");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-390-compact.png` });
  });

  test("German, Russian and Arabic fit on a phone", async ({ page }) => {
    for (const lang of ["de", "ru", "ar"]) {
      const root = await open(page, { lang });
      await expectNoOverflow(page, root, `${lang} phone`);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/many-390-${lang}.png` });
    }
  });
});
