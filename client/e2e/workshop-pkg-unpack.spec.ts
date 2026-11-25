// #933: a game in asset packs made into a .pkg. "Unpack the asset packs" is ticked by default,
// "Leave out the AMPR emulator" sits under it unticked (and goes away when unpacking is off), and
// both reach the engine's package build. A plain game is offered neither.
import { expect, test } from "@playwright/test";

test("a packed game's package is unpacked by default and can leave the emulator out", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1400 });
  const builds: Record<string, unknown>[] = [];
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  const inspection = (source: string) => ({
    source: `folder ${source}`,
    files: 3,
    bytes: 3 << 20,
    content_id: "UP9000-PPSA01234_00-PACKEDGAME000000",
    title: source.endsWith("Packed") ? "Packed Game" : "Plain Game",
    planned_size: 3 << 20,
    output_free: 1 << 40,
    checks: [{ name: "eboot.bin", ok: true, detail: "ok" }],
    ...(source.endsWith("Packed") ? { packs: "zstd" } : {}),
  });
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
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });

  // A plain game: nothing to unpack.
  const source = main.locator("#fpkg-source");
  await source.fill("/games/Plain");
  await source.blur();
  await expect(main.getByText("Plain Game").first()).toBeVisible();
  await expect(main.getByTestId("unpack-option")).toHaveCount(0);

  // A packed game: unpacked unless unticked; the emulator stays unless left out.
  await source.fill("/games/Packed");
  await source.blur();
  await expect(main.getByText("Packed Game").first()).toBeVisible();
  const unpack = main.getByTestId("unpack-option").getByRole("checkbox");
  await expect(unpack).toBeChecked();
  const leaveOut = main.getByTestId("leave-out-ampr-emu").getByRole("checkbox");
  await expect(leaveOut).not.toBeChecked();
  await unpack.uncheck();
  await expect(main.getByTestId("leave-out-ampr-emu")).toHaveCount(0);
  await unpack.check();
  await leaveOut.check();

  await main.getByRole("button", { name: "Convert only" }).first().click();
  await expect.poll(() => builds.length).toBe(1);
  expect(builds[0]).toMatchObject({ source: "/games/Packed", unpack_packs: true, leave_out_ampr_emu: true });
});
