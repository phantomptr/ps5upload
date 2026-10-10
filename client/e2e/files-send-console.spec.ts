import { expect, test, type Route } from "@playwright/test";

// #433: select files in Files, "Send to another console…", pick the console and folder, and each
// item is queued on that console as a copy from this one. The engine here is a fake.

const PRO = "192.168.0.5";
const PHAT = "192.168.0.6";

test("Files sends the selection to another console, one queued copy per item", async ({ page }) => {
  await page.addInitScript(
    ([pro, phat]) => {
      window.localStorage.setItem("ps5upload.host", pro);
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({
          profiles: [
            { id: "pro", name: "Pro", host: pro },
            { id: "phat", name: "Phat", host: phat },
          ],
          active_id: "pro",
        }),
      );
    },
    [PRO, PHAT],
  );
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  const relays: { from: string; src: string; to: string; dest: string }[] = [];
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      const addr = url.searchParams.get("addr") ?? "";
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
      if (p === "/api/ps5/list-dir") {
        const onPhat = addr.startsWith(PHAT);
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: onPhat
            ? [{ name: "b.ffpfsc", kind: "file", size: 1, mtime: 1 }]
            : [
                { name: "GameA-app", kind: "dir", size: 0, mtime: 1 },
                { name: "b.ffpfsc", kind: "file", size: 4_000_000, mtime: 1 },
              ],
          truncated: false,
          total_scanned: 2,
          returned: 2,
        });
      }
      if (p === "/api/transfer/ps5-to-ps5") {
        relays.push(req.postDataJSON());
        return json(route, { job_id: `j${relays.length}` });
      }
      // Each copy finishes at once, so the second (run after the first, on Phat's queue) starts.
      if (p.startsWith("/api/jobs")) return json(route, { status: "done", bytes_sent: 10, total_bytes: 10, elapsed_ms: 5 });
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(main.getByText("GameA-app", { exact: true })).toBeVisible({ timeout: 30_000 });
  await main.getByTitle("Select all").check();
  await main.getByTestId("fs-send-console").click();

  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("radio", { name: /Phat/ })).toHaveAttribute("aria-checked", "true");
  // What is already there on Phat is named before anything runs.
  await expect(dialog.getByTestId("send-console-summary")).toContainText("Already there and replaced: b.ffpfsc");
  await dialog.getByTestId("send-console-confirm").click();
  await expect(dialog).toHaveCount(0);

  await expect.poll(() => relays.length, { timeout: 20_000 }).toBe(2);
  expect(relays.map((r) => r.dest).sort()).toEqual(["/data/GameA-app", "/data/b.ffpfsc"]);
  expect(relays.every((r) => r.from.startsWith(PRO) && r.to.startsWith(PHAT))).toBe(true);
});
