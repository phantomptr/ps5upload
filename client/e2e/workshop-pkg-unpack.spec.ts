// #933: a game in asset packs made into a .pkg. "Unpack the asset packs" is ticked by default,
// "Leave out the AMPR emulator" sits under it unticked (and goes away when unpacking is off), and
// both reach the engine's package build. A plain game is offered neither.
// The packed game's own ampr_emu.index is stale: that is a warning only while the package would
// keep the packs (and the index with them), not while it unpacks them and gets a fresh one.
// The two options are findable: first under ③ in a group named "Asset packs and AMPR", with one
// line in that place before a game is checked and for a game in no packs, on a desktop and on a
// phone, and in a waiting game's own choices in the queue.
import { expect, test, type Locator, type Page } from "@playwright/test";

/** PS5UPLOAD_SHOTS=<folder>: also save a picture of each state there (the card or panel named,
 *  whole, however far down the page it is). */
const SHOTS = process.env.PS5UPLOAD_SHOTS;
const shot = async (what: Locator, name: string) => {
  if (SHOTS) await what.screenshot({ path: `${SHOTS}/${name}.png` });
};
/** Card ③, by its title. */
const optionsCard = (main: Locator) => main.locator("xpath=.//div[normalize-space(text())='③ Options']/..");

const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });

const inspection = (source: string) => ({
  source: `folder ${source}`,
  files: 3,
  bytes: 3 << 20,
  content_id: "UP9000-PPSA01234_00-PACKEDGAME000000",
  title: source.endsWith("Packed") ? "Packed Game" : "Plain Game",
  planned_size: 3 << 20,
  output_free: 1 << 40,
  checks: [
    { name: "eboot.bin", ok: true, detail: "ok" },
    ...(source.endsWith("Packed")
      ? [
          {
            name: "ampr_emu.index for a libSceAmpr title",
            ok: false,
            detail: "the folder's ampr_emu.index is stale (7 listed missing, 0 unlisted, 10 sizes differ)",
            applies_to: "packs_kept",
          },
        ]
      : []),
  ],
  ...(source.endsWith("Packed") ? { packs: "zstd" } : {}),
});

/** The engine, as far as this page asks it: a check of each game, and the builds it is sent. */
async function engine(page: Page): Promise<Record<string, unknown>[]> {
  const builds: Record<string, unknown>[] = [];
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const body = req.postDataJSON?.() as Record<string, unknown> | null;
      if (path === "/api/fpkg/inspect") return route.fulfill(json(inspection(String(body?.source ?? ""))));
      if (path === "/api/fpkg/estimate") return route.fulfill(json({ error: "no" }, 500));
      if (path === "/api/fpkg/build") {
        builds.push(body ?? {});
        return route.fulfill(json({ job_id: `job-${builds.length}` }));
      }
      if (/^\/api\/jobs\/[^/]+$/.test(path))
        return route.fulfill(
          json({
            status: "done",
            started_at_ms: 0,
            completed_at_ms: 1,
            elapsed_ms: 1,
            tx_id_hex: "PPSA01234",
            bytes_sent: 10,
            dest: "/out/PPSA01234.pkg",
          }),
        );
      return route.fulfill(json({ error: "down" }, 503));
    },
  );
  return builds;
}

const top = async (l: Locator) => (await l.boundingBox())!.y;

test("a packed game's package is unpacked by default and can leave the emulator out", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1400 });
  const builds = await engine(page);
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });

  // No game checked yet: one line under ③ says where the options for a packed game will be.
  const note = main.getByTestId("pkg-packs-note");
  await expect(note).toHaveAttribute("data-state", "pending");
  await expect(note).toContainText("Asset packs and AMPR");
  await expect(note).toContainText("Check a game under ① first");
  await expect(note).toContainText("leave out the AMPR emulator");
  await shot(optionsCard(main), "desktop-1-not-checked");

  // A plain game: nothing to unpack, and it says so.
  const source = main.locator("#fpkg-source");
  await source.fill("/games/Plain");
  await source.blur();
  await expect(main.getByText("Plain Game").first()).toBeVisible();
  await expect(main.getByTestId("unpack-option")).toHaveCount(0);
  await expect(note).toHaveAttribute("data-state", "none");
  await expect(note).toContainText("This game has no asset packs, so there is nothing to unpack.");
  await shot(optionsCard(main), "desktop-2-plain-game");

  // A packed game: unpacked unless unticked; the emulator stays unless left out.
  await source.fill("/games/Packed");
  await source.blur();
  await expect(main.getByText("Packed Game").first()).toBeVisible();
  const group = main.getByRole("group", { name: "Asset packs and AMPR" });
  await expect(group).toBeVisible();
  await expect(note).toHaveCount(0);
  const unpack = main.getByTestId("unpack-option").getByRole("checkbox");
  await expect(unpack).toBeChecked();
  const leaveOut = main.getByTestId("leave-out-ampr-emu").getByRole("checkbox");
  await expect(leaveOut).not.toBeChecked();
  // Both sit above the output folder and the compression tiles.
  expect(await top(main.getByTestId("leave-out-ampr-emu"))).toBeLessThan(await top(main.locator("#fpkg-output")));
  expect(await top(group)).toBeLessThan(await top(main.getByRole("radiogroup").last()));
  await shot(optionsCard(main), "desktop-3-packed-game");
  // Unpacked, the package gets a fresh index: the game's stale one is no warning.
  const stale = main.getByText(/ampr_emu\.index is stale/);
  await expect(stale).toBeHidden();
  await expect(main.getByText("Checks passed: 1")).toBeVisible();
  await unpack.uncheck();
  await expect(main.getByTestId("leave-out-ampr-emu")).toHaveCount(0);
  // With the packs kept, the stale index goes into the package: said under the game.
  await expect(stale).toBeVisible();
  await unpack.check();
  await expect(stale).toBeHidden();
  await leaveOut.check();

  await main.getByRole("button", { name: "Convert only" }).first().click();
  await expect.poll(() => builds.length).toBe(1);
  expect(builds[0]).toMatchObject({ source: "/games/Packed", unpack_packs: true, leave_out_ampr_emu: true });
});

test("on a phone the options for a packed game come first under ③, and the queue has them too", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const builds = await engine(page);
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });
  const note = main.getByTestId("pkg-packs-note");
  await expect(note).toHaveAttribute("data-state", "pending");
  await expect(note).toContainText("Asset packs and AMPR");
  await shot(optionsCard(main), "phone-1-not-checked");

  const source = main.locator("#fpkg-source");
  await source.fill("/games/Plain");
  await source.blur();
  await expect(main.getByText("Plain Game").first()).toBeVisible();
  await expect(note).toHaveAttribute("data-state", "none");
  await shot(optionsCard(main), "phone-2-plain-game");

  await source.fill("/games/Packed");
  await source.blur();
  await expect(main.getByText("Packed Game").first()).toBeVisible();
  const group = main.getByRole("group", { name: "Asset packs and AMPR" });
  await expect(group).toBeVisible();
  // Right under the card's title: nothing else of ③ has to be scrolled past to reach them.
  const title = main.getByText("③ Options");
  const groupTop = await top(group);
  expect(groupTop - (await top(title))).toBeLessThan(60);
  expect(groupTop).toBeLessThan(await top(main.locator("#fpkg-output")));
  expect(await top(main.getByTestId("leave-out-ampr-emu"))).toBeLessThan(await top(main.getByRole("radiogroup").last()));
  // Nothing spills past the screen's edge.
  for (const l of [group, main.getByTestId("unpack-option"), main.getByTestId("leave-out-ampr-emu")]) {
    const box = (await l.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
  }
  await shot(optionsCard(main), "phone-3-packed-game");

  // Into the queue with the emulator left out: its own choices show both, and can change them.
  await main.getByTestId("leave-out-ampr-emu").getByRole("checkbox").check();
  await main.getByRole("button", { name: "Add this game to the queue" }).click();
  await main.getByRole("button", { name: "This game's choices" }).click();
  const options = main.getByTestId("queue-item-options");
  const queued = options.getByRole("group", { name: "Asset packs and AMPR" });
  await expect(queued).toBeVisible();
  await expect(options.getByTestId("unpack-option").getByRole("checkbox")).toBeChecked();
  const queuedLeaveOut = options.getByTestId("leave-out-ampr-emu").getByRole("checkbox");
  await expect(queuedLeaveOut).toBeChecked();
  await shot(options, "phone-4-queue-choices");
  await queuedLeaveOut.uncheck();
  await expect(queuedLeaveOut).not.toBeChecked();
  await expect(options.getByTestId("unpack-option").getByRole("checkbox")).toBeChecked();
  expect(builds).toHaveLength(0);
});
