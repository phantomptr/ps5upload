import { expect, test, type Page } from "@playwright/test";
import { routeEngine } from "./fixtures/engine";

// #937: a package picked on Upload & Install shows the name it gives its game in a field. Changing
// it rewrites the name in the package (the engine's /api/pkg/name), and the screen reads the
// package again before it is installed. An official package says why its name stays.

const MARKED = "A Game-example.invalid-abc";

interface Fake {
  /** The name the package holds now. */
  name: string;
  refusal: { code: string; reason: string } | null;
  /** The package's own room for a name, in bytes (absent: the engine's overall limit). */
  room?: { default_name: number; all_languages: number };
  /** Every POST /api/pkg/name body, in order. */
  renames: unknown[];
  /** How many times the package's header was read. */
  headerReads: number;
}

async function fakeEngine(page: Page, fake: Fake) {
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "12.6.30", caps: {} };
    if (path === "/api/ps5/status") return { ok: true };
    if (path === "/api/ps5/volumes") return { volumes: [] };
    if (path === "/api/files/upload") {
      const name = /filename="([^"]+)"/.exec(req.postDataBuffer()?.toString("latin1") ?? "")?.[1] ?? "x";
      return { upload_id: `up-${name}`, path: `/tmp/staged/${name}`, size: 5, filename: name };
    }
    if (path === "/api/pkg/parse-split") {
      fake.headerReads += 1;
      return {
        head: { content_id: "UP0000-PPSA01234_00-TESTGAME00000000", title: fake.name, category: "gd", platform: "ps5" },
        parts: [],
        total_size: 1 << 30,
      };
    }
    if (path === "/api/pkg/name" && req.method() === "GET") {
      return {
        platform: "ps5",
        content_id: "UP0000-PPSA01234_00-TESTGAME00000000",
        name: fake.name,
        entries: [
          { key: "en-US", name: fake.name },
          { key: "ja-JP", name: "テストゲーム" },
        ],
        other_names: 1,
        refusal: fake.refusal,
        max_bytes: 127,
        ...(fake.room ? { room: fake.room } : {}),
      };
    }
    if (path === "/api/pkg/name" && req.method() === "POST") {
      const body = req.postDataJSON() as { name: string };
      fake.renames.push(body);
      const previous = fake.name;
      fake.name = body.name.trim();
      return { platform: "ps5", previous, name: fake.name, changed: 1, written_bytes: 2236 };
    }
    return {};
  });
  await page.goto("/send", { waitUntil: "domcontentloaded" });
}

async function pickPackage(page: Page) {
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles([
    { name: "GameA.pkg", mimeType: "application/octet-stream", buffer: Buffer.from("GameA") },
  ]);
}

test("a picked package's name is changed before it is installed", async ({ page }) => {
  const fake: Fake = { name: MARKED, refusal: null, renames: [], headerReads: 0 };
  await fakeEngine(page, fake);
  await pickPackage(page);

  const card = page.getByTestId("pkg-name-card");
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card).toContainText("Name on the PS5");
  const input = card.getByTestId("pkg-name-input");
  await expect(input).toHaveValue(MARKED);
  const save = card.getByTestId("pkg-name-save");
  await expect(save).toBeDisabled();
  await expect(card).toContainText("Use this name in every language (other names in the package: 1)");

  const readsBefore = fake.headerReads;
  await input.fill("A Game");
  await expect(save).toBeEnabled();
  await save.click();
  await expect(card.getByTestId("pkg-name-saved")).toHaveText("Name changed.");
  expect(fake.renames).toEqual([{ path: "/tmp/staged/GameA.pkg", name: "A Game", all_languages: false }]);
  await expect(input).toHaveValue("A Game");
  await expect(save).toBeDisabled();
  // The screen reads the package again: what it installs is the renamed package.
  await expect.poll(() => fake.headerReads).toBeGreaterThan(readsBefore);
  await expect(page.getByTestId("upload-stream-install")).toBeVisible();

  // Every language, on request.
  await card.getByRole("checkbox").check();
  await expect(save).toBeEnabled();
  await save.click();
  await expect.poll(() => fake.renames.length).toBe(2);
  expect(fake.renames[1]).toEqual({ path: "/tmp/staged/GameA.pkg", name: "A Game", all_languages: true });

  await card.screenshot({ path: process.env.PKG_NAME_SHOT ?? "/dev/null" }).catch(() => {});

  // No horizontal overflow at phone width.
  await page.setViewportSize({ width: 380, height: 800 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);

});

// A name longer than the package has room for (in bytes, as the engine counts) is said in the
// field itself, in the app's language, and never sent.
test("a name too long for the package is refused in the field, before anything is sent", async ({ page }) => {
  const fake: Fake = {
    name: MARKED,
    refusal: null,
    renames: [],
    headerReads: 0,
    room: { default_name: 30, all_languages: 30 },
  };
  await fakeEngine(page, fake);
  await pickPackage(page);
  const card = page.getByTestId("pkg-name-card");
  await expect(card).toBeVisible({ timeout: 20_000 });
  const input = card.getByTestId("pkg-name-input");
  const save = card.getByTestId("pkg-name-save");
  await expect(input).toHaveValue(MARKED);
  await expect(card.getByTestId("pkg-name-room")).toHaveCount(0);

  await input.fill("A Game");
  await expect(card.getByTestId("pkg-name-room")).toHaveText("Bytes left for the name: 24");
  await expect(save).toBeEnabled();

  // Eleven Japanese letters: 33 bytes, three more than fit.
  await input.fill("テ".repeat(11));
  const tooLong = card.getByTestId("pkg-name-too-long");
  await expect(tooLong).toContainText("This name is too long for this package. Bytes that fit: 30. Bytes over: 3.");
  await expect(save).toBeDisabled();
  await input.press("Enter");
  await page.waitForTimeout(200);
  expect(fake.renames).toEqual([]);
  await card.screenshot({ path: process.env.PKG_NAME_LONG_SHOT ?? "/dev/null" }).catch(() => {});

  // Ten fit exactly.
  await input.fill("テ".repeat(10));
  await expect(card.getByTestId("pkg-name-room")).toHaveText("Bytes left for the name: 0");
  await expect(save).toBeEnabled();
  await save.click();
  await expect.poll(() => fake.renames.length).toBe(1);
});

test("an official package shows its name and why it can't be changed", async ({ page }) => {
  const fake: Fake = {
    name: "An Official Game",
    refusal: { code: "retail", reason: "this is an official package" },
    renames: [],
    headerReads: 0,
  };
  await fakeEngine(page, fake);
  await pickPackage(page);
  const card = page.getByTestId("pkg-name-card");
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card).toContainText("An Official Game");
  await expect(card.getByTestId("pkg-name-refusal")).toContainText("This is an official package, so its name can't be changed");
  await expect(card.getByTestId("pkg-name-input")).toHaveCount(0);
  await expect(card.getByTestId("pkg-name-save")).toHaveCount(0);
  expect(fake.renames).toEqual([]);
});
