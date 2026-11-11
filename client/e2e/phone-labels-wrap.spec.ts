import { expect, test, type Locator, type Page, type Route } from "@playwright/test";

// #794: on a phone, labels next to buttons were squeezed to a word, or a letter, a line: the label
// had a zero flex basis, so its row never wrapped the buttons below it. Each label here gets room.

const HOST = "192.168.0.5";

test.use({ viewport: { width: 412, height: 915 }, hasTouch: true });

async function setUp(page: Page) {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const p = new URL(route.request().url()).pathname;
      if (p === "/api/version") return json(route, { caps: {}, version: "12.4.5" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          max_transfer_streams: 8,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          started_at_unix: 1791360198,
          ucred_elevated: true,
          version: "12.4.5",
        });
      if (p === "/api/ps5/console-shares") return json(route, { shares: [] });
      if (p === "/api/remote/connections") return json(route, { connections: [] });
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );
}

const main = (page: Page) => page.locator("[data-scroll-root]:visible");

async function width(l: Locator): Promise<number> {
  await expect(l).toBeVisible({ timeout: 30_000 });
  return (await l.boundingBox())?.width ?? 0;
}

async function noSidewaysScroll(page: Page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
}

test("Home: the Network shares hint is not squeezed beside its button", async ({ page }) => {
  await setUp(page);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const hint = main(page).getByText(/^Pick games straight from your NAS/);
  expect(await width(hint)).toBeGreaterThan(200);
  await noSidewaysScroll(page);
});

test("Shared Folders: the Servers description and the PS5 shares title are not squeezed", async ({ page }) => {
  await setUp(page);
  await page.goto("/shared-folders", { waitUntil: "domcontentloaded" });

  const desc = main(page).getByText(/^Your NAS and servers \(SMB/);
  expect(await width(desc)).toBeGreaterThan(200);

  // The card title with Refresh and "Add a share to the PS5" beside it: one line, buttons below.
  const title = main(page).getByText("Shares on the PS5", { exact: true });
  expect(await width(title)).toBeGreaterThan(100);
  const titleBox = await title.boundingBox();
  expect(titleBox?.height ?? 999).toBeLessThan(40);

  // The add button stays inside the screen.
  const add = main(page).getByRole("button", { name: "Add a share to the PS5" });
  await expect(add).toBeVisible();
  const addBox = await add.boundingBox();
  expect((addBox?.x ?? 0) + (addBox?.width ?? 0)).toBeLessThanOrEqual(412);
  await noSidewaysScroll(page);
});

// #794 again: the Files queue row's name was a column a few letters wide ("PAU/SE/D B/eat./Sab…")
// while "Resume" and "Remove from queue" kept the rest of the line.
test("Files: a paused queue row's name has the row, its buttons go below", async ({ page }) => {
  const NAME = "Beat.Saber_CUSA12878_v1.00_[5.50]_OPOISSO893.pkg";
  await setUp(page);
  await page.addInitScript(
    ({ host, name }) => {
      window.localStorage.setItem(
        "ps5upload.fsUploadQueue.v1",
        JSON.stringify({
          [host]: {
            items: [
              {
                id: "q1",
                request: {
                  queueId: "q1",
                  host,
                  addr: `${host}:9120`,
                  destDir: "/data",
                  srcPaths: [],
                  steps: [{ kind: "one", src: `/home/me/${name}`, name, txId: "tx1" }],
                },
                allowedExisting: [],
                paused: "user",
              },
            ],
            paused: true,
            running: false,
            error: null,
            currentId: null,
          },
        }),
      );
    },
    { host: HOST, name: NAME },
  );
  await page.goto("/files", { waitUntil: "domcontentloaded" });

  const queue = page.getByTestId("fs-upload-queue");
  const text = queue.getByTestId("fs-upload-queue-text");
  expect(await width(text)).toBeGreaterThan(200);

  // "Paused" stays one word on one line.
  const chipBox = await queue.getByTestId("fs-upload-paused").boundingBox();
  expect(chipBox?.height ?? 999).toBeLessThan(24);

  // The buttons are below the name, inside the screen.
  const textBox = await text.boundingBox();
  for (const name of ["Resume", "Remove from queue"]) {
    const b = await queue.getByRole("button", { name }).first().boundingBox();
    expect(b?.y ?? 0).toBeGreaterThanOrEqual((textBox?.y ?? 0) + (textBox?.height ?? 0) - 1);
    expect((b?.x ?? 0) + (b?.width ?? 0)).toBeLessThanOrEqual(412);
  }
  await noSidewaysScroll(page);
});
