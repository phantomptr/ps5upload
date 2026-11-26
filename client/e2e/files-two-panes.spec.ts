import { expect, test, type Locator, type Page } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// #952: Files' two panes. The PS5 on one side, this computer (here the web UI, so the computer the
// engine runs on), a saved server or another console on the other, and Copy / Move between them.
// The engine is a fake that keeps a small file tree for each place, so what a transfer does shows.

const SHOTS = process.env.PS5UPLOAD_SHOTS ?? "";
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

interface Entry {
  name: string;
  dir: boolean;
  size: number;
}

/** The fake engine: three places, what each holds, and what was asked of it in order. */
function fakeEngine() {
  const ps5: Record<string, Entry[]> = {
    "/data": [
      { name: "homebrew", dir: true, size: 0 },
      { name: "notes.txt", dir: false, size: 4096 },
    ],
    "/data/homebrew": [{ name: "tool.elf", dir: false, size: 900 }],
  };
  const pc: Record<string, Entry[]> = {
    "/srv": [
      { name: "games", dir: true, size: 0 },
      { name: "patch.pkg", dir: false, size: 1234 },
    ],
    "/srv/games": [{ name: "big.exfat", dir: false, size: 77 }],
  };
  const nas: Record<string, Entry[]> = { "/": [{ name: "backups", dir: true, size: 0 }], "/backups": [] };
  const log: string[] = [];
  const jobs: Record<string, { status: "running" | "done" | "failed"; land?: () => void }> = {};
  let n = 0;
  /** How a new download ends: at once ("done"), never until released ("hold"), or "fail". */
  const state = { download: "done" as "done" | "hold" | "fail" };
  const land = (tree: Record<string, Entry[]>, dir: string, e: Entry) => {
    tree[dir] = [...(tree[dir] ?? []).filter((x) => x.name !== e.name), e];
  };
  const entryAt = (path: string) => {
    const i = path.lastIndexOf("/");
    const dir = i <= 0 ? "/" : path.slice(0, i);
    return { dir, e: ps5[dir]?.find((x) => x.name === path.slice(i + 1)) };
  };
  return {
    ps5,
    pc,
    nas,
    log,
    jobs,
    state,
    async install(page: Page) {
      await routeEngine(page, (path, req) => {
        const url = new URL(req.url());
        if (path === "/api/version") return { version: "12.6.33", caps: {} };
        if (path === "/api/ps5/status")
          return { ok: true, version: "12.6.33", ucred_elevated: true, ps5_kernel: "releases/10.00", instance_id: 1, command_count: 1 };
        if (path === "/api/ps5/volumes")
          return { volumes: [{ path: "/data", fs_type: "ufs", total_bytes: 825e9, free_bytes: 410e9, writable: true }] };
        if (path === "/api/ps5/files/prefs") return { supported: true, favorites: [], recent: [], last: null };
        if (path === "/api/ps5/list-dir") {
          const dir = url.searchParams.get("path") ?? "/data";
          const list = ps5[dir] ?? [];
          return {
            path: dir,
            entries: list.map((e) => ({ name: e.name, kind: e.dir ? "dir" : "file", size: e.size, mtime: 1 })),
            truncated: false,
            total_scanned: list.length,
            returned: list.length,
          };
        }
        if (path === "/api/local/storage-roots") return ["/srv"];
        if (path === "/api/local/list-dir") {
          const dir = url.searchParams.get("path") ?? "/srv";
          return (pc[dir] ?? []).map((e) => ({ name: e.name, path: `${dir}/${e.name}`, is_dir: e.dir, size: e.size }));
        }
        if (path === "/api/local/path-kind") return { kind: "file" };
        if (path === "/api/remote/connections")
          return {
            connections: [
              { id: "nas", name: "NAS", protocol: "smb", host: "10.0.0.9", port: 445, share: "games", user: "", start_path: "/", host_key: null, has_secret: false },
            ],
          };
        if (path === "/api/remote/list") {
          const raw = (req.postDataJSON() as { path: string }).path;
          const dir = "/" + raw.replace(/^remote:\/\/nas\/?/, "");
          return {
            entries: (nas[dir] ?? []).map((e) => ({ name: e.name, is_dir: e.dir, size: e.size, mtime: null })),
            next_cursor: null,
          };
        }
        if (path === "/api/transfer/download") {
          const body = req.postDataJSON() as { src_path: string; dest_dir: string; remove_source?: boolean };
          const id = `dl${++n}`;
          log.push(`download ${body.src_path} -> ${body.dest_dir}${body.remove_source ? " (engine move)" : ""}`);
          const { dir: srcDir, e } = entryAt(body.src_path);
          const landIt = () => {
            if (!e) return;
            if (body.dest_dir.startsWith("remote://nas")) land(nas, "/" + body.dest_dir.replace(/^remote:\/\/nas\/?/, ""), e);
            else land(pc, body.dest_dir, e);
            // The engine's move (#953) deletes the source itself once the copy checked out.
            if (body.remove_source) ps5[srcDir] = (ps5[srcDir] ?? []).filter((x) => x.name !== e.name);
          };
          jobs[id] = { status: state.download === "fail" ? "failed" : state.download === "hold" ? "running" : "done", land: landIt };
          if (jobs[id].status === "done") landIt();
          return { job_id: id };
        }
        if (/^\/api\/transfer\/(file|file-list|dir)$/.test(path)) {
          log.push(`upload ${JSON.stringify(req.postDataJSON())}`);
          return { job_id: `up${++n}` };
        }
        if (path.startsWith("/api/jobs/")) {
          const id = path.split("/")[3];
          const j = jobs[id];
          if (!j) return { status: "done", bytes_sent: 1, total_bytes: 1 };
          return j.status === "failed"
            ? { status: "failed", error: "disk full" }
            : { status: j.status, bytes_sent: j.status === "done" ? 10 : 3, total_bytes: 10 };
        }
        if (path === "/api/jobs") return [];
        if (path === "/api/ps5/fs/delete") {
          const p = (req.postDataJSON() as { path: string }).path;
          log.push(`delete ${p}`);
          const { dir } = entryAt(p);
          ps5[dir] = (ps5[dir] ?? []).filter((x) => `${dir}/${x.name}` !== p);
          return { ok: true };
        }
        return undefined;
      });
    },
    /** Ends a held download as done, its file landing where it went. */
    finish(id: string) {
      jobs[id].status = "done";
      jobs[id].land?.();
    },
  };
}

async function openFiles(page: Page, opts: { lang?: string; twoPanes?: boolean } = {}) {
  await seedConsole(page);
  await page.addInitScript(
    ([lang, on]) => {
      if (lang) window.localStorage.setItem("ps5upload.lang", lang);
      if (on && !window.sessionStorage.getItem("seeded")) {
        window.localStorage.setItem("ps5upload.files.twoPanes", "1");
        window.sessionStorage.setItem("seeded", "1");
      }
    },
    [opts.lang ?? "", opts.twoPanes ? "1" : ""] as const,
  );
  const engine = fakeEngine();
  await engine.install(page);
  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(main.locator("[data-fs-name='notes.txt']")).toBeVisible({ timeout: 60_000 });
  return { main, engine };
}

const other = (page: Page) => page.getByTestId("two-panes-other");
const row = (scope: Locator, name: string) => scope.locator(`[data-fs-name="${name}"]`);
const otherRow = (page: Page, name: string) => other(page).locator(`[data-other-row="${name}"]`);

async function shot(page: Page, name: string) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

for (const viewport of [
  { width: 1280, height: 800 },
  { width: 1600, height: 900 },
]) {
  test.describe(`${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport });

    test("the toggle opens the other side, both sides browse on their own, and it is remembered", async ({ page }) => {
      const { main } = await openFiles(page);
      const toggle = main.getByTestId("fs-two-panes-toggle");
      await expect(toggle).toHaveAttribute("aria-pressed", "false");
      await expect(page.locator("[data-two-panes]")).toHaveCount(0);
      await shot(page, `off-${viewport.width}`);

      await toggle.click();
      await expect(toggle).toHaveAttribute("aria-pressed", "true");
      await expect(other(page).getByTestId("two-panes-place")).toHaveValue("computer");
      await expect(otherRow(page, "patch.pkg")).toBeVisible();
      // The PS5 side is on the left, the other side on the right, the actions between.
      const ps5Box = (await page.locator("[data-pane='ps5']").boundingBox())!;
      const otherBox = (await other(page).boundingBox())!;
      const actionsBox = (await page.getByTestId("two-panes-actions").boundingBox())!;
      expect(ps5Box.x + ps5Box.width).toBeLessThanOrEqual(actionsBox.x + 1);
      expect(actionsBox.x + actionsBox.width).toBeLessThanOrEqual(otherBox.x + 1);

      // Each side opens its own folders.
      await otherRow(page, "games").getByRole("button", { name: "games" }).click();
      await expect(otherRow(page, "big.exfat")).toBeVisible();
      await expect(other(page).getByTestId("two-panes-path")).toHaveText("/srv/games");
      await row(main, "homebrew").locator("[data-fs-open]").click();
      await expect(row(main, "tool.elf")).toBeVisible();
      await expect(other(page).getByTestId("two-panes-path")).toHaveText("/srv/games");
      await shot(page, `on-${viewport.width}`);

      // Kept for the next visit, with the other side's folder.
      await page.reload();
      await expect(main.getByTestId("fs-two-panes-toggle")).toHaveAttribute("aria-pressed", "true", { timeout: 60_000 });
      await expect(other(page).getByTestId("two-panes-path")).toHaveText("/srv/games");
      await main.getByTestId("fs-two-panes-toggle").click();
      await page.reload();
      await expect(main.getByTestId("fs-two-panes-toggle")).toHaveAttribute("aria-pressed", "false", { timeout: 60_000 });
      await expect(page.locator("[data-two-panes]")).toHaveCount(0);
    });
  });
}

test.describe("transfers", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("computer → PS5 is Add files into the PS5 folder; no Move from the computer", async ({ page }) => {
    const { main, engine } = await openFiles(page, { twoPanes: true });
    await expect(otherRow(page, "patch.pkg")).toBeVisible();
    await expect(page.getByTestId("two-panes-move-ps5")).toBeDisabled();
    await expect(page.getByTestId("two-panes-actions")).toContainText("never deletes files on its computer");
    await expect(page.getByTestId("two-panes-copy-ps5")).toBeDisabled();
    await otherRow(page, "patch.pkg").getByRole("checkbox").check();
    await page.getByTestId("two-panes-copy-ps5").click();
    await expect.poll(() => engine.log.find((l) => l.startsWith("upload")), { timeout: 20_000 }).toContain("/srv/patch.pkg");
    expect(engine.log.find((l) => l.startsWith("upload"))).toContain("/data");
    expect(engine.log.some((l) => l.startsWith("delete"))).toBe(false);
    await expect(main.getByTestId("fs-two-panes-toggle")).toBeVisible();
  });

  test("PS5 → saved server is the download to that server, into the folder shown", async ({ page }) => {
    const { main, engine } = await openFiles(page, { twoPanes: true });
    await other(page).getByTestId("two-panes-place").selectOption({ label: "NAS" });
    await expect(otherRow(page, "backups")).toBeVisible();
    await otherRow(page, "backups").getByRole("button", { name: "backups" }).click();
    await expect(other(page).getByTestId("two-panes-path")).toHaveText("/backups");
    // From the server to the PS5 copies; a move would need the server to delete.
    await expect(page.getByTestId("two-panes-move-ps5")).toBeDisabled();
    await expect(page.getByTestId("two-panes-actions")).toContainText("doesn't delete files on saved servers");
    await row(main, "notes.txt").getByRole("checkbox").check();
    await page.getByTestId("two-panes-copy-other").click();
    await expect.poll(() => engine.log.filter((l) => l.startsWith("download"))).toEqual([
      "download /data/notes.txt -> remote://nas/backups",
    ]);
    // The copy shows on the server's side when it ends; the PS5 keeps its file.
    await expect(otherRow(page, "notes.txt")).toBeVisible({ timeout: 20_000 });
    await expect(row(main, "notes.txt")).toBeVisible();
    expect(engine.log.some((l) => l.startsWith("delete"))).toBe(false);
    await shot(page, "server-copied");
  });

  test("Move PS5 → computer deletes the PS5 file only after its copy finished and is there", async ({ page }) => {
    const { main, engine } = await openFiles(page, { twoPanes: true });
    await expect(otherRow(page, "patch.pkg")).toBeVisible();
    engine.state.download = "hold";
    await row(main, "notes.txt").getByRole("checkbox").check();
    await page.getByTestId("two-panes-move-other").click();
    const ask = page.getByRole("alertdialog");
    await expect(ask).toContainText("only after its copy is there in full");
    await shot(page, "move-confirm");
    await ask.getByRole("button", { name: "Move", exact: true }).click();
    await expect.poll(() => engine.log.filter((l) => l.startsWith("download")).length).toBe(1);
    // The copy is still running: nothing deleted.
    await page.waitForTimeout(1500);
    expect(engine.log.some((l) => l.startsWith("delete"))).toBe(false);
    engine.finish("dl1");
    await expect.poll(() => engine.log, { timeout: 20_000 }).toEqual([
      "download /data/notes.txt -> /srv",
      "delete /data/notes.txt",
    ]);
    await expect(row(main, "notes.txt")).toHaveCount(0, { timeout: 20_000 });
    await expect(otherRow(page, "notes.txt")).toBeVisible();
  });

  test("a ticked folder can't Move to this computer (Copy only), and says why", async ({ page }) => {
    const { main } = await openFiles(page, { twoPanes: true });
    await expect(otherRow(page, "patch.pkg")).toBeVisible();
    await row(main, "notes.txt").getByRole("checkbox").check();
    await expect(page.getByTestId("two-panes-move-other")).toBeEnabled();
    await row(main, "homebrew").getByRole("checkbox").check();
    await expect(page.getByTestId("two-panes-move-other")).toBeDisabled();
    await expect(page.getByTestId("two-panes-copy-other")).toBeEnabled();
    await expect(page.getByTestId("two-panes-actions")).toContainText("Move takes files only here");
    // The PS5 side's selection bar is narrower than the window: its actions wrap, none cut or
    // over another.
    const boxes = await main
      .getByTestId("fs-selection-actions")
      .locator(":scope > button")
      .evaluateAll((els) =>
        els.map((el) => {
          const r = el.getBoundingClientRect();
          return { l: r.left, r: r.right, t: r.top, b: r.bottom, cut: el.scrollWidth > el.clientWidth + 1 };
        }),
      );
    expect(boxes.filter((b) => b.cut)).toEqual([]);
    for (let i = 0; i < boxes.length; i++)
      for (let j = i + 1; j < boxes.length; j++) {
        const [a, b] = [boxes[i], boxes[j]];
        const overlap = a.l < b.r - 1 && b.l < a.r - 1 && a.t < b.b - 1 && b.t < a.b - 1;
        expect(overlap, `buttons ${i} and ${j} overlap`).toBe(false);
      }
    await shot(page, "folder-copy-only");
  });

  test("Move PS5 → saved server is the engine's own move: files and folders, no delete from the app", async ({ page }) => {
    const { main, engine } = await openFiles(page, { twoPanes: true });
    await other(page).getByTestId("two-panes-place").selectOption({ label: "NAS" });
    await expect(otherRow(page, "backups")).toBeVisible();
    await row(main, "homebrew").getByRole("checkbox").check();
    await row(main, "notes.txt").getByRole("checkbox").check();
    await expect(page.getByTestId("two-panes-move-other")).toBeEnabled();
    await page.getByTestId("two-panes-move-other").click();
    await page.getByRole("alertdialog").getByRole("button", { name: "Move", exact: true }).click();
    await expect.poll(() => engine.log, { timeout: 20_000 }).toEqual([
      "download /data/homebrew -> remote://nas (engine move)",
      "download /data/notes.txt -> remote://nas (engine move)",
    ]);
    await expect(row(main, "notes.txt")).toHaveCount(0, { timeout: 20_000 });
    await expect(row(main, "homebrew")).toHaveCount(0);
    await expect(otherRow(page, "homebrew")).toBeVisible();
    await expect(otherRow(page, "notes.txt")).toBeVisible();
  });

  test("a failed copy in a move keeps the PS5 file", async ({ page }) => {
    const { main, engine } = await openFiles(page, { twoPanes: true });
    await expect(otherRow(page, "patch.pkg")).toBeVisible();
    engine.state.download = "fail";
    await row(main, "notes.txt").getByRole("checkbox").check();
    await page.getByTestId("two-panes-move-other").click();
    await page.getByRole("alertdialog").getByRole("button", { name: "Move", exact: true }).click();
    await expect.poll(() => engine.log.filter((l) => l.startsWith("download")).length).toBe(1);
    await page.waitForTimeout(2000);
    expect(engine.log.some((l) => l.startsWith("delete"))).toBe(false);
    await expect(row(main, "notes.txt")).toBeVisible();
  });

  test("Tab goes from a row on one side to the list on the other; Enter opens a folder there", async ({ page }) => {
    const { main } = await openFiles(page, { twoPanes: true });
    await expect(otherRow(page, "games")).toBeVisible();
    await row(main, "notes.txt").getByRole("checkbox").focus();
    await page.keyboard.press("Tab");
    await expect(otherRow(page, "games")).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(other(page).getByTestId("two-panes-path")).toHaveText("/srv/games");
    // Enter there did not open anything on the PS5 side.
    await expect(row(main, "notes.txt")).toBeVisible();
    await otherRow(page, "big.exfat").focus();
    await page.keyboard.press("Tab");
    await expect(row(main, "homebrew")).toBeFocused();
  });

  test("right to left: the PS5 side is on the right, the other side on the left", async ({ page }) => {
    await openFiles(page, { twoPanes: true, lang: "ar" });
    await expect(otherRow(page, "patch.pkg")).toBeVisible();
    const ps5Box = (await page.locator("[data-pane='ps5']").boundingBox())!;
    const otherBox = (await other(page).boundingBox())!;
    expect(otherBox.x + otherBox.width).toBeLessThanOrEqual(ps5Box.x + 1);
    await shot(page, "rtl-ar");
  });
});

test.describe("wide font", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  for (const lang of ["en", "de", "ru"]) {
    test(`${lang}: the toggle and the Copy and Move buttons show their whole names`, async ({ page }) => {
      const { main } = await openFiles(page, { twoPanes: true, lang });
      await page.addStyleTag({ content: WIDE_FONT });
      await expect(other(page).locator("[data-other-row]").first()).toBeVisible();
      const cut = await page
        .locator("[data-testid='two-panes-actions'] button > span, [data-testid='fs-two-panes-toggle'] > span")
        .evaluateAll((els) =>
          els.filter((el) => el.scrollWidth > el.clientWidth + 1).map((el) => el.textContent),
        );
      expect(cut).toEqual([]);
      // The header still holds its buttons on the row (nothing runs off the page).
      const toggle = (await main.getByTestId("fs-two-panes-toggle").boundingBox())!;
      expect(toggle.x + toggle.width).toBeLessThanOrEqual(1280);
      await shot(page, `wide-font-${lang}`);
    });
  }
});

for (const viewport of [
  { width: 390, height: 844 },
  { width: 820, height: 1180 },
]) {
  test.describe(`${viewport.width} wide, touch`, () => {
    test.use({ viewport, hasTouch: true, isMobile: true });

    test("Files stays one list: no toggle, no second side, even when it was left on", async ({ page }) => {
      await openFiles(page, { twoPanes: true });
      await expect(page.getByTestId("fs-two-panes-toggle")).toHaveCount(0);
      await expect(page.locator("[data-two-panes]")).toHaveCount(0);
      await shot(page, `touch-${viewport.width}`);
    });
  });
}
