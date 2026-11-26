import { expect, test, type Page, type Request } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// Payloads › Catalog › Your payload sources, against a made-up engine: add a GitHub project, pick
// its file, download it (the card then shows the release, size and SHA-256), send it after a
// confirm that names the file, and remove the source. On a desktop, on a phone, and with a wide
// font. Nothing reaches GitHub or a console.
//
// PS5UPLOAD_SHOTS=<dir> also saves screenshots.

const SHOTS = process.env.PS5UPLOAD_SHOTS;
// A wide font, as Linux CI and some Android phones have (the label specs do the same).
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

const ID = "custom-0a1b2c3d4e5f";
const URL_ = "https://github.com/someone/a-payload-with-a-rather-long-project-name";
const FILE = "a-payload-with-a-rather-long-project-name-v1.6.0.elf";
const SHA = "3f9a".repeat(16);
const PATH = `/home/me/.local/share/ps5upload/payloads/${ID}/${FILE}`;

const preview = {
  id: ID,
  name: "a-payload-with-a-rather-long-project-name",
  kind: "github",
  project_url: URL_,
  already_added: false,
  current_file: "",
  releases: [
    {
      tag: "v1.6.0",
      name: "v1.6.0",
      published_at: "2026-11-01T00:00:00Z",
      prerelease: false,
      html_url: `${URL_}/releases/tag/v1.6.0`,
      files: [
        { name: FILE, size: 412_000, pattern: "a-payload-with-a-rather-long-project-name-v*.elf" },
        { name: "debug-build.elf", size: 900_000, pattern: "debug-build.elf" },
      ],
    },
  ],
};

const base = {
  id: ID,
  name: preview.name,
  kind: "github",
  project_url: URL_,
  owner: "someone",
  repo: preview.name,
  file_name: FILE,
  file_pattern: "a-payload-with-a-rather-long-project-name-v*.elf",
  update_available: false,
};
const latest = {
  tag: "v1.6.0",
  release_name: "v1.6.0",
  file_name: FILE,
  size: 412_000,
  published_at: "2026-11-01T00:00:00Z",
  prerelease: false,
  html_url: `${URL_}/releases/tag/v1.6.0`,
};
const have = {
  tag: "v1.6.0",
  file_name: FILE,
  size: 412_000,
  sha256: SHA,
  published_at: "2026-11-01T00:00:00Z",
  downloaded_at: 1_800_000_000,
  checksum: "matched",
  path: PATH,
};

interface Engine {
  asked: string[];
  sent: Array<Record<string, unknown>>;
}

/** A made-up engine that keeps the source list: none, added, downloaded, removed. */
async function open(page: Page, opts: { lang?: string } = {}): Promise<Engine> {
  const engine: Engine = { asked: [], sent: [] };
  let state: "none" | "added" | "downloaded" = "none";
  const current = () =>
    state === "none" ? [] : [{ ...base, latest, ...(state === "downloaded" ? { have } : {}) }];
  await seedConsole(page);
  if (opts.lang) await page.addInitScript((l) => window.localStorage.setItem("ps5upload.lang", l), opts.lang);
  await routeEngine(page, async (path, req: Request) => {
    const method = req.method();
    if (path.startsWith("/api/payloads/repos")) engine.asked.push(`${method} ${path}`);
    if (path === "/api/version") return { version: "12.6.34", caps: {} };
    if (path === "/api/ps5/status") return { ok: true };
    if (path === "/api/payloads") return { payloads: [], local: [] };
    if (path === "/api/payloads/sources") return { sources: [] };
    if (path === "/api/payloads/repos" && method === "GET") return { sources: current() };
    if (path === "/api/payloads/repos/preview") return preview;
    if (path === "/api/payloads/repos" && method === "POST") {
      state = "added";
      return { ...base };
    }
    if (path === `/api/payloads/repos/${ID}/download`) {
      state = "downloaded";
      return current()[0];
    }
    if (path === `/api/payloads/repos/${ID}` && method === "DELETE") {
      state = "none";
      return { removed: true };
    }
    if (path === "/api/payload/send") {
      engine.sent.push(JSON.parse(req.postData() ?? "{}"));
      return { status: "sent", bytes: 412_000 };
    }
    return undefined;
  });
  await page.goto("/payloads?tab=catalog", { waitUntil: "domcontentloaded" });
  return engine;
}

function section(page: Page) {
  return page.locator("[data-scroll-root]:visible").getByTestId("payload-sources");
}

/** Nothing wider than the page, and no text out of the section's cards. */
async function expectFits(page: Page, what: string) {
  const sideways = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(sideways, `${what}: sideways scroll`).toBeLessThanOrEqual(0);
  const out = await section(page)
    .locator("[data-source-card], [data-testid='psrc-add']")
    .evaluateAll((boxes) =>
      boxes
        .filter((box) => {
          const r = box.getBoundingClientRect();
          return [...box.querySelectorAll("span, dd, dt, button, label, p, h3")].some((el) => {
            const b = el.getBoundingClientRect();
            return b.width > 0 && (b.left < r.left - 1 || b.right > r.right + 1);
          });
        })
        .map((box) => box.textContent?.slice(0, 40)),
    );
  expect(out, `${what}: text outside its card`).toEqual([]);
  // No button label is cut.
  const cut = await section(page)
    .locator("button span")
    .evaluateAll((els) =>
      els.filter((el) => el.scrollWidth > el.clientWidth + 1).map((el) => el.textContent),
    );
  expect(cut, `${what}: cut labels`).toEqual([]);
}

async function addPickDownloadSendRemove(page: Page, engine: Engine, shot: string) {
  const s = section(page);
  await expect(s.getByRole("heading", { name: "Your payload sources" })).toBeVisible({ timeout: 60_000 });
  await expect(s.getByText("Add a GitHub, Gitea or Forgejo project")).toBeVisible();
  // Opening Payloads is a check: the list is asked for with check=1.
  await expect.poll(() => engine.asked.filter((a) => a.startsWith("GET /api/payloads/repos")).length).toBeGreaterThan(0);

  // Add: the address, then its files, then the pick.
  await s.getByRole("button", { name: "Add a payload source" }).click();
  const form = s.getByTestId("psrc-add");
  await form.getByRole("textbox").fill(URL_);
  await form.getByRole("button", { name: "Show its files" }).click();
  await expect(form.getByText("Pick the payload file (release v1.6.0):")).toBeVisible();
  await expect(form.getByRole("note")).toHaveText(
    "PS5Upload will download and send whatever this project publishes under that name. Only add projects you trust.",
  );
  await expect(form.getByRole("button", { name: "Add source" })).toBeDisabled();
  await form.getByRole("radio", { name: new RegExp(FILE.replace(/\./g, "\\.")) }).check();
  await expect(form.getByText("Newer releases: a file named like a-payload-with-a-rather-long-project-name-v*.elf")).toBeVisible();
  await expectFits(page, `${shot} add`);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sources-${shot}-add.png` });
  await form.getByRole("button", { name: "Add source" }).click();

  // The card: the file, not downloaded yet, Download.
  const card = s.locator(`[data-source-card="${ID}"]`);
  await expect(card).toBeVisible();
  await expect(card.getByText("Not downloaded yet. Newest: v1.6.0")).toBeVisible();
  await card.getByRole("button", { name: "Download" }).click();

  // Downloaded: exactly what would go to the console.
  await expect(card.getByTestId("psrc-sha256")).toHaveText(SHA);
  await expect(card.getByText("Matches the checksum its release lists.")).toBeVisible();
  await expect(card.getByRole("button", { name: "Download" })).toHaveCount(0);
  await expectFits(page, `${shot} card`);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sources-${shot}-card.png` });

  // Send asks first, naming the file, its release and SHA-256; Cancel sends nothing.
  await card.getByRole("button", { name: "Send to PS5" }).click();
  const ask = page.getByRole("alertdialog");
  await expect(ask).toContainText(`Send ${FILE} to 192.168.1.100?`);
  await expect(ask).toContainText(`SHA-256: ${SHA}`);
  await expect(ask).toContainText("Release: v1.6.0");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sources-${shot}-send.png` });
  await ask.getByRole("button", { name: "Cancel" }).click();
  expect(engine.sent).toHaveLength(0);
  await card.getByRole("button", { name: "Send to PS5" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Send to PS5" }).click();
  await expect.poll(() => engine.sent.length).toBe(1);
  expect(engine.sent[0]).toMatchObject({ host: "192.168.1.100", path: PATH });

  // Remove asks, then the card goes.
  await card.getByRole("button", { name: "Remove source" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Remove source" }).click();
  await expect(card).toHaveCount(0);
  expect(engine.asked).toContain(`DELETE /api/payloads/repos/${ID}`);
}

test.describe("desktop", () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test("add, pick the file, download, send after a confirm, remove", async ({ page }) => {
    const engine = await open(page);
    await addPickDownloadSendRemove(page, engine, "1280");
  });

  test("with a wide font", async ({ page }) => {
    const engine = await open(page);
    await page.addStyleTag({ content: WIDE_FONT });
    await addPickDownloadSendRemove(page, engine, "1280-wide");
  });

  test("a bad address is said plainly, and the built-in catalogue has no Download", async ({ page }) => {
    await seedConsole(page);
    await routeEngine(page, (path) => {
      if (path === "/api/version") return { version: "12.6.34", caps: {} };
      if (path === "/api/payloads/repos") return { sources: [] };
      return undefined;
    });
    // The engine refuses an http:// address with its code.
    await page.route("**/api/payloads/repos/preview", (route) =>
      route.fulfill({
        status: 400,
        contentType: "application/json",
        body: JSON.stringify({ code: "url_not_https", error: "Only https:// addresses can be added." }),
      }),
    );
    await page.goto("/payloads?tab=catalog", { waitUntil: "domcontentloaded" });
    const s = section(page);
    await s.getByRole("button", { name: "Add a payload source" }).click({ timeout: 60_000 });
    await s.getByRole("textbox").fill("http://github.com/someone/thing");
    await s.getByRole("button", { name: "Show its files" }).click();
    await expect(s.getByRole("alert")).toHaveText("Only https:// addresses can be added.");
  });

  test("a rate limit says when to try again", async ({ page }) => {
    await seedConsole(page);
    await routeEngine(page, (path) => {
      if (path === "/api/version") return { version: "12.6.34", caps: {} };
      if (path === "/api/payloads/repos")
        return {
          sources: [
            {
              ...base,
              error: { code: "rate_limited", message: "GitHub is limiting", retry_at: 1_900_000_000 },
            },
          ],
        };
      return undefined;
    });
    await page.goto("/payloads?tab=catalog", { waitUntil: "domcontentloaded" });
    const card = section(page).locator(`[data-source-card="${ID}"]`);
    await expect(card.getByRole("alert")).toContainText("Too many checks for now. Try again after", { timeout: 60_000 });
  });
});

test.describe("phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test("add, pick the file, download, send after a confirm, remove", async ({ page }) => {
    const engine = await open(page);
    await addPickDownloadSendRemove(page, engine, "390");
  });

  test("with a wide font, in German", async ({ page }) => {
    const engine = await open(page, { lang: "de" });
    await page.addStyleTag({ content: WIDE_FONT });
    const s = section(page);
    await expect(s.getByRole("heading", { name: "Deine Payload-Quellen" })).toBeVisible({ timeout: 60_000 });
    await s.getByRole("button", { name: "Payload-Quelle hinzufügen" }).click();
    const form = s.getByTestId("psrc-add");
    await form.getByRole("textbox").fill(URL_);
    await form.getByRole("button", { name: "Dateien anzeigen" }).click();
    await form.getByRole("radio").first().check();
    await expectFits(page, "390 de add");
    await form.getByRole("button", { name: "Quelle hinzufügen" }).click();
    const card = s.locator(`[data-source-card="${ID}"]`);
    await card.getByRole("button", { name: "Herunterladen" }).click();
    await expect(card.getByTestId("psrc-sha256")).toHaveText(SHA);
    await expectFits(page, "390 de card");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/sources-390-de-wide.png` });
    expect(engine.asked).toContain(`POST /api/payloads/repos/${ID}/download`);
  });
});
