import { expect, test, type Route } from "@playwright/test";

// #953: select items in Files, "Move to…", pick a saved server and a folder on it: each item goes
// to the engine as a download into that folder with remove_source (the engine deletes the original
// only after it checked the copy), one after another. "Copy there…" sends the same without it.
// The engine here is a fake.

const PRO = "192.168.0.5";

async function setup(page: import("@playwright/test").Page) {
  await page.addInitScript((pro) => {
    window.localStorage.setItem("ps5upload.host", pro);
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({ profiles: [{ id: "pro", name: "Pro", host: pro }], active_id: "pro" }),
    );
  }, PRO);
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  const downloads: Record<string, unknown>[] = [];
  let running = 0;
  let overlapped = false;
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: { rar: true }, version: "6.7.4" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60",
          ucred_elevated: true,
          version: "6.7.4",
        });
      if (p === "/api/ps5/volumes")
        return json(route, {
          volumes: [
            {
              path: "/data",
              fs_type: "nullfs",
              total_bytes: 900e9,
              free_bytes: 400e9,
              allocatable_bytes: 398e9,
              writable: true,
              is_placeholder: false,
            },
          ],
        });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [
            { name: "GameA-app", kind: "dir", size: 0, mtime: 1 },
            { name: "b.bin", kind: "file", size: 4_000_000, mtime: 1 },
          ],
          truncated: false,
          total_scanned: 2,
          returned: 2,
        });
      if (p === "/api/remote/connections")
        return json(route, {
          connections: [
            { id: "nas1", name: "Home NAS", protocol: "smb", host: "10.0.0.5", port: 445, share: "games", user: "", start_path: "/", host_key: null, has_secret: false },
          ],
        });
      if (p === "/api/remote/list") {
        const at = (JSON.parse(req.postData() ?? "{}") as { path: string }).path;
        return json(route, {
          entries: at.endsWith("/PS5") ? [] : [{ name: "PS5", is_dir: true, size: 0, mtime: null }],
          next_cursor: null,
        });
      }
      if (p === "/api/transfer/download") {
        if (running) overlapped = true;
        running++;
        downloads.push(req.postDataJSON());
        return json(route, { job_id: `j${downloads.length}` });
      }
      // Each job ends at once, so the next item starts after it.
      if (p.startsWith("/api/jobs/")) {
        running = 0;
        return json(route, { status: "done", bytes_sent: 10, total_bytes: 10, elapsed_ms: 5, dest: "x" });
      }
      return json(route, {});
    },
  );
  return { downloads, overlapped: () => overlapped };
}

for (const how of ["Move", "Copy"] as const) {
  test(`Files ${how === "Move" ? "moves" : "copies"} the selection to a folder on a saved server`, async ({ page }) => {
    const seen = await setup(page);
    await page.goto("/files", { waitUntil: "domcontentloaded" });
    const main = page.getByRole("main");
    await expect(main.getByText("GameA-app", { exact: true })).toBeVisible({ timeout: 30_000 });
    await main.getByTitle("Select all").check();
    await main.getByRole("button", { name: "Move to…" }).first().click();

    const servers = page.getByTestId("move-servers");
    await expect(servers).toContainText("Home NAS");
    await servers.getByRole("button", { name: `${how} there…` }).click();

    const picker = page.getByRole("dialog");
    await picker.getByText("PS5", { exact: true }).click();
    await picker.getByRole("button", { name: "Use this folder" }).click();

    await expect.poll(() => seen.downloads.length, { timeout: 20_000 }).toBe(2);
    expect(seen.overlapped()).toBe(false);
    expect(seen.downloads.map((d) => [d.src_path, d.kind]).sort()).toEqual([
      ["/data/GameA-app", "folder"],
      ["/data/b.bin", "file"],
    ]);
    for (const d of seen.downloads) {
      expect(d.dest_dir).toBe("remote://nas1/PS5");
      expect(d.remove_source).toBe(how === "Move" ? true : undefined);
    }
  });
}
