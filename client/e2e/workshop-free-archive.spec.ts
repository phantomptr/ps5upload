// Workshop's one dangerous choice for an archive: "Free the archive's space while it unpacks",
// which deletes the archive on the way. It is off for every game, the engine is asked whether
// and how it can be done before anything else, the user is asked with what is lost if it goes
// wrong, and only a yes reaches the unpack. A kind of archive that can only be deleted at the
// end says so; one that can't be freed safely says why and stays off.
import { expect, test } from "@playwright/test";

test("an archive's space is freed only after the check, the warning and a yes", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1400 });
  const checks: Record<string, unknown>[] = [];
  const extracts: Record<string, unknown>[] = [];
  const cleanups: Record<string, unknown>[] = [];
  const builds: Record<string, unknown>[] = [];
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  const done = (dest: string) => ({
    status: "done",
    started_at_ms: 0,
    completed_at_ms: 1,
    elapsed_ms: 1,
    tx_id_hex: "",
    bytes_sent: 10,
    dest,
  });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const body = req.postDataJSON?.() as Record<string, unknown> | null;
      const source = String(body?.source ?? "");
      if (path === "/api/fpkg/extract/free-check") {
        checks.push(body ?? {});
        if (source.endsWith("Net.zip"))
          return route.fulfill(
            json({ error: "/dl/Net.zip: it is on a network drive, where freeing space as it goes is not safe" }, 400),
          );
        return route.fulfill(
          json({
            mode: source.endsWith(".7z") ? "end" : "entries",
            game: "folder",
            parts: 1,
            archive_bytes: 10 << 20,
            unpacked: 20 << 20,
            same_drive: true,
            needed: 30 << 20,
            free: 1 << 40,
          }),
        );
      }
      if (path === "/api/fpkg/extract") {
        extracts.push(body ?? {});
        return route.fulfill(json({ job_id: `unpack-${extracts.length}` }, 202));
      }
      if (path === "/api/fpkg/extract/cleanup") {
        cleanups.push(body ?? {});
        return route.fulfill(json({ ok: true }));
      }
      if (path === "/api/fpkg/build") {
        builds.push(body ?? {});
        return route.fulfill(json({ job_id: `build-${builds.length}` }));
      }
      if (path === "/api/jobs/unpack-1") return route.fulfill(json(done("/out/Game.unpacked/Game")));
      if (path === "/api/jobs/unpack-2") return route.fulfill(json(done("/out/.ps5upload-extract-2/Plain")));
      if (/^\/api\/jobs\/build-\d+$/.test(path)) return route.fulfill(json(done("/out/PPSA01234.pkg")));
      return route.fulfill(json({ error: "down" }, 503));
    },
  );
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });

  const source = main.locator("#fpkg-source");
  await source.fill("/dl/Game.zip");
  await source.blur();
  const option = main.getByTestId("free-archive-option");
  const toggle = option.getByRole("switch");
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(option.getByTestId("free-archive-danger")).toHaveText("Dangerous");
  expect(checks).toHaveLength(0);

  // Turning it on asks the engine, then the user: what it does, the risk, the advice.
  await toggle.click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toContainText("Delete this archive while it unpacks?");
  await expect(dialog).toContainText("frees the archive's space while it unpacks");
  await expect(dialog).toContainText("the archive is already gone and can't be brought back");
  await expect(dialog).toContainText("you keep whatever was unpacked so far");
  await expect(dialog).toContainText("Only use this for an archive you can get again");
  await expect(dialog).toContainText("A .zip is freed file by file as it unpacks.");
  await expect(dialog.getByTestId("confirm-details")).toContainText("/dl/Game.zip");
  expect(checks).toHaveLength(1);
  expect(checks[0]).toMatchObject({ source: "/dl/Game.zip" });

  // No: it stays off, and nothing is sent.
  await dialog.getByTestId("confirm-cancel").click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(option.getByTestId("free-archive-path")).toHaveCount(0);

  // Yes: on for this archive, and the card says what will go.
  await toggle.click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete it while it unpacks" }).click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await expect(option.getByTestId("free-archive-path")).toHaveText("Will be deleted while it unpacks: /dl/Game.zip");

  await main.getByRole("button", { name: "Convert only" }).first().click();
  await expect.poll(() => extracts.length).toBe(1);
  expect(extracts[0]).toMatchObject({ source: "/dl/Game.zip", free_archive: true });
  await expect.poll(() => builds.length).toBe(1);
  expect(builds[0]).toMatchObject({ source: "/out/Game.unpacked/Game" });
  // Made: the folder that held the game goes, asked for by name.
  await expect.poll(() => cleanups.length).toBe(1);
  expect(cleanups[0]).toEqual({ path: "/out/Game.unpacked/Game", kept: true });

  // The next archive starts with it off, and unpacks the usual way.
  await main.getByRole("button", { name: /Convert another game/ }).click();
  await source.fill("/dl/Plain.zip");
  await source.blur();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await main.getByRole("button", { name: "Convert only" }).first().click();
  await expect.poll(() => extracts.length).toBe(2);
  expect(extracts[1]).toMatchObject({ source: "/dl/Plain.zip" });
  expect(extracts[1]).not.toHaveProperty("free_archive");
  await expect.poll(() => cleanups.length).toBe(2);
  expect(cleanups[1]).toEqual({ path: "/out/.ps5upload-extract-2/Plain" });

  // A .7z can't be freed bit by bit: the question says it is deleted once unpacked.
  await main.getByRole("button", { name: /Convert another game/ }).click();
  await source.fill("/dl/Other.7z");
  await source.blur();
  await toggle.click();
  const atEnd = page.getByRole("alertdialog");
  await expect(atEnd).toContainText("Delete this archive once it is unpacked?");
  await expect(atEnd).toContainText("can't be freed bit by bit");
  await atEnd.getByRole("button", { name: "Delete it once it is unpacked" }).click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await expect(option.getByTestId("free-archive-path")).toHaveText(
    "Will be deleted as soon as it is unpacked: /dl/Other.7z",
  );
  await expect(option.getByTestId("free-archive-end-note")).toBeVisible();
  // Turning it off needs no question.
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(page.getByRole("alertdialog")).toHaveCount(0);

  // An archive that can't be freed safely: no question, the reason, and it stays off.
  await source.fill("/dl/Net.zip");
  await source.blur();
  await toggle.click();
  await expect(option.getByTestId("free-archive-refused")).toContainText("Not available:");
  await expect(option.getByTestId("free-archive-refused")).toContainText("network drive");
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  expect(extracts).toHaveLength(2);
});
