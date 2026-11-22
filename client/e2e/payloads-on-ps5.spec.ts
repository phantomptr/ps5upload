import { expect, test, type Page } from "@playwright/test";

// "On your PS5" for the #846 reporter's console: Payload Manager keeps four payloads, three of
// them run (kstuff as kstuff.elf, Kraken Mount as elfldr's generic payload.elf), and elfldr runs
// on its own. Each running saved payload shows once, on its Payload Manager row; elfldr is under
// "Also running"; nothing says Payload Manager was started "outside Payload Manager".
//
// PS5UPLOAD_SHOTS=<dir> also saves screenshots at 1280 and 412 wide.

const HOST = "192.168.1.100";
const SHOTS = process.env.PS5UPLOAD_SHOTS;

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
  proc(103, "payload.elf"),
  proc(104, "ps5upload.elf", { is_self: true }),
  proc(106, "elfldr.elf"),
  proc(60, "SceShellUI", { kind: "system" }),
];

const item = (stem: string, filename: string) => ({
  slot: `local/${stem}`,
  path: `/data/pldmgr/payloads/local/${stem}/${filename}`,
  kind: "local",
  filename,
});
const PM_ITEMS = [
  item("kstuff-1.13-fpkg", "kstuff-1.13-fpkg.elf"),
  item("kraken-mount", "kraken-mount.elf"),
  item("syslang-zh-Hans-0.3", "syslang-zh-Hans-0.3.elf"),
  item("AnyPad-PS5-0.9.1-beta", "AnyPad-PS5-0.9.1-beta.elf"),
];

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
];

async function mockConsole(page: Page) {
  await page.addInitScript((host) => window.localStorage.setItem("ps5upload.host", host), HOST);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const path = url.pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "12.6.3", caps: {} };
      else if (path === "/api/ps5/status") body = { ok: true };
      else if (path === "/api/ps5/process/list") body = { processes: PROCESSES, truncated: false };
      else if (path === "/api/payloads") body = { payloads: CATALOG, local: [] };
      else if (path === "/api/ps5/pldmgr") {
        body = {
          present: true,
          version: "0.6.1",
          payloads: PM_ITEMS.map((p) => ({
            ...p,
            folder: p.slot,
            name: "",
            version: "",
            source: "",
            install_source: "local",
          })),
          autoload: {
            enabled: true,
            delay_s: 5,
            list: ["local/syslang-zh-Hans-0.3", "local/kstuff-1.13-fpkg", "!2000", "local/kraken-mount"],
          },
        };
      } else if (path === "/api/ps5/smp/status") {
        body = {
          installed: false,
          running: false,
          mounted_images: [],
          errors: [],
          mounter: "kraken",
          kraken: {
            installed: true,
            running: true,
            api: true,
            version: "0.2.0",
            waiting: null,
            games: [],
            title_ids: [],
            ampr: null,
            settings: { inject_ampr: false, ampr_auto_update: false, scan_paths: [] },
            scan_roots: [],
            errors: [],
          },
        };
      } else if (path === "/api/ps5/list-dir") {
        const dir = url.searchParams.get("path");
        body = {
          entries:
            dir === "/data"
              ? [{ name: "pldmgr", kind: "dir" }, { name: "krakenmount", kind: "dir" }, { name: "kstuff.elf", kind: "file" }]
              : dir === "/data/pldmgr"
                ? [{ name: "pldmgr_config.txt", kind: "file" }]
                : [],
          truncated: false,
        };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
}

test("On your PS5 shows each saved running payload once, on its Payload Manager row", async ({ page }) => {
  await mockConsole(page);
  await page.setViewportSize({ width: 1280, height: 1400 });
  await page.goto("/payloads?tab=ps5&open=payload-manager", { waitUntil: "domcontentloaded" });

  const root = page.locator("[data-scroll-root]:visible");
  await expect(root.getByText("4 saved · 2 running")).toBeVisible({ timeout: 30_000 });
  // Payload Manager, its four saved payloads and elfldr, each counted once.
  await expect(root.getByText("6 on this PS5")).toBeVisible();

  // The saved payloads are the tab's main list, not inside Payload Manager's card: its Manage
  // holds its own settings and points up to the list.
  const saved = root.getByTestId("pm-saved-list");
  await expect(saved.getByText("Your payloads · 4")).toBeVisible();
  await expect(saved.locator("li")).toHaveCount(4);
  const pmPanel = root.locator('[data-card="payload-manager"]');
  await expect(pmPanel.getByText("Load payloads at boot")).toBeVisible();
  await expect(pmPanel.getByTestId("pm-saved-elsewhere")).toBeVisible();
  await expect(pmPanel.getByTestId("pm-running-origin")).toHaveCount(0);
  await expect(root.getByTestId("hub-detected")).toContainText("Found on this PS5");

  // kstuff and Kraken Mount on their rows, with the boot place the list gives them.
  const origins = root.getByTestId("pm-running-origin");
  await expect(origins).toHaveCount(2);
  await expect(origins.nth(0)).toContainText("From your Payload Manager list, starts at boot #2");
  await expect(origins.nth(1)).toContainText("From your Payload Manager list, starts at boot #3");
  await expect(root.getByText("Running · PID 102")).toBeVisible();
  await expect(root.getByText("Running · PID 103")).toBeVisible();

  // Not a second time as cards; elfldr alone under Also running.
  await expect(root.locator('[data-card="kstuff-echostretch"]')).toHaveCount(0);
  await expect(root.locator('[data-card="kraken-mount"]')).toHaveCount(0);
  const also = root.getByTestId("hub-also-running");
  await expect(also.locator("[data-card]")).toHaveCount(1);
  await expect(also.locator('[data-card="proc:elfldr"]')).toBeVisible();
  await expect(also).toContainText("Not in your Payload Manager list.");

  // Payload Manager's own card never says it was started outside itself.
  const pmCard = root.locator('[data-card="payload-manager"]');
  await expect(pmCard.getByTestId("hub-origin").first()).toHaveText("Started by your jailbreak or an autoloader.");
  await expect(root.getByText("In the boot list, not found")).toHaveCount(0);

  if (SHOTS) {
    await page.screenshot({ path: `${SHOTS}/on-ps5-1280.png` });
    await also.scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${SHOTS}/on-ps5-1280-also.png` });
    await page.setViewportSize({ width: 412, height: 2400 });
    await root.evaluate((el) => el.scrollTo(0, 0));
    await expect(root.getByText("4 saved · 2 running")).toBeVisible();
    await page.screenshot({ path: `${SHOTS}/on-ps5-412.png` });
    await also.scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${SHOTS}/on-ps5-412-also.png` });
  }
  // No sideways scroll on a phone.
  await page.setViewportSize({ width: 412, height: 900 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});
