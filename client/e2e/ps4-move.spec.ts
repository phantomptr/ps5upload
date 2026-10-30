import { expect, test } from "@playwright/test";

// Move saves from a PS4 (9.9): the PS4 is only listed and read over its FTP server; the chosen
// games are copied to this computer first; on the PS5 only a game whose PS4 save folder already
// exists for the chosen user is replaced (by the engine, which backs the PS5's save up first),
// and only once the account IDs are known to match.

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "9.9.0",
};

test("a PS4's saves are copied here, then put on the PS5 user with the same account ID", async ({ page }) => {
  const remoteWrites: string[] = [];
  const fetched: string[] = [];
  const imports: unknown[] = [];
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const json = (b: unknown, s = 200) =>
        route.fulfill({ status: s, contentType: "application/json", body: JSON.stringify(b) });
      if (path === "/api/version") return json({ version: "9.9.0", caps: {} });
      if (path === "/api/ps5/status") return json(status);
      if (path === "/api/save-backup")
        return json({
          settings: { enabled: false, dir: "/backups", keep: 5, hosts: [] },
          status: { running: false, last_at: 0, backed_up: 0, failed: 0, last_error: null },
        });
      if (path === "/api/remote/connections") {
        if (req.method() === "POST") {
          remoteWrites.push("connection");
          return json({ id: "c1", name: "PS4 192.168.1.50", protocol: "ftp", host: "192.168.1.50", port: 2121, share: "", user: "", start_path: "/", host_key: null, has_secret: false });
        }
        return json({ connections: [] });
      }
      if (path === "/api/remote/list") {
        const p = (req.postDataJSON() as { path: string }).path;
        const dir = (name: string) => ({ name, is_dir: true, size: 0, mtime: null });
        const file = (name: string, size: number) => ({ name, is_dir: false, size, mtime: null });
        const listing: Record<string, unknown[]> = {
          "remote://c1/user/home": [dir("1de82450"), dir("00000000x"), dir("10000001")],
          "remote://c1/user/home/1de82450/savedata": [dir("CUSA00001"), dir("CUSA00002")],
          "remote://c1/user/home/10000001/savedata": [],
          "remote://c1/user/home/1de82450/savedata/CUSA00001": [file("sdimg_SAVEDATA00", 1000), file("SAVEDATA00.bin", 96)],
          "remote://c1/user/home/1de82450/savedata/CUSA00002": [file("sdimg_SAVEDATA00", 2000), file("SAVEDATA00.bin", 96)],
        };
        return json({ entries: listing[p] ?? [], next_cursor: null });
      }
      if (path === "/api/remote/fetch") {
        const b = req.postDataJSON() as { path: string; dest_dir: string };
        fetched.push(b.path);
        return json({ job_id: `j${fetched.length}` });
      }
      if (path.startsWith("/api/jobs/")) {
        const n = Number(path.split("/j").pop());
        const title = fetched[n - 1].split("/").pop()!;
        const bytes = title === "CUSA00001" ? 1096 : 2096;
        return json({ status: "done", dest: `/backups/PS4/${title}`, bytes_sent: bytes, total_bytes: bytes });
      }
      if (path === "/api/profile/info")
        return json({
          ok: true,
          uid: 268435456,
          uid_hex: "0x10000000",
          username: "Alice",
          users: [{ uid: 268435456, uid_hex: "0x10000000", username: "Alice" }],
          slots: [{ slot: 1, name: "Alice", type: "", flags: 0, id: "0x7a356e99a9e2205c", activated: true, offline_activated: false }],
        });
      if (path === "/api/ps5/saves/list")
        return json({
          saves: [
            // Only CUSA00001 has a PS4 save folder on this user already.
            { title_id: "CUSA00001", user_id: 268435456, path: "/user/home/10000000/savedata/CUSA00001", size: 1, mtime: 1, kind: "ps4" },
            { title_id: "PPSA01234", user_id: 268435456, path: "/user/home/10000000/savedata_prospero/PPSA01234", size: 1, mtime: 1, kind: "ps5" },
          ],
        });
      if (path === "/api/save-backup/import-ps4") {
        imports.push(req.postDataJSON());
        return json({ ok: true });
      }
      if (path.startsWith("/api/remote/")) remoteWrites.push(path);
      return json({});
    },
  );

  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  await page.getByTestId("ps4-move-open").click({ timeout: 30_000 });
  await page.getByTestId("ps4-move-host").fill("192.168.1.50");
  await page.getByTestId("ps4-move-connect").click();

  // The user with saves (folders that are not users, and users without saves, are not offered).
  await expect(page.getByTestId("ps4-move-user")).toHaveValue("1de82450");
  await expect(page.getByTestId("ps4-move-games")).toContainText("CUSA00001");
  await expect(page.getByTestId("ps4-move-games")).toContainText("CUSA00002");
  await page.getByTestId("ps4-move-backup").click();
  await expect.poll(() => fetched).toEqual([
    "remote://c1/user/home/1de82450/savedata/CUSA00001",
    "remote://c1/user/home/1de82450/savedata/CUSA00002",
  ]);

  // Onto the PS5: the account check first.
  const step3 = page.getByTestId("ps4-move-ps5");
  await expect(step3).toContainText("0x7a356e99a9e2205c");
  const put = page.getByTestId("ps4-move-put");
  await expect(put).toBeDisabled();
  await page.getByTestId("ps4-move-ps4-account").fill("0x1111");
  await expect(page.getByTestId("ps4-move-mismatch")).toBeVisible();
  await expect(put).toBeDisabled();
  await page.getByTestId("ps4-move-ps4-account").fill("7A356E99A9E2205C");
  await expect(step3).toContainText("The IDs match.");
  // Only the game already on this user is put; the other says how to make it ready.
  const targets = page.getByTestId("ps4-move-targets");
  await expect(targets).toContainText("CUSA00002");
  await expect(targets).toContainText("start the game on the PS5 once");
  await expect(put).toHaveText("Put 1 on the PS5");
  await put.click();
  await page.getByRole("button", { name: "Put on the PS5" }).click();
  await expect.poll(() => imports).toEqual([
    {
      addr: "192.168.1.100",
      title_id: "CUSA00001",
      save_path: "/user/home/10000000/savedata/CUSA00001",
      user_id: 268435456,
      src_dir: "/backups/PS4/CUSA00001",
    },
  ]);
  await expect(targets).toContainText("Done");
  // The PS4 was only read: one saved connection, no other writes to it.
  expect(remoteWrites).toEqual(["connection"]);
});
