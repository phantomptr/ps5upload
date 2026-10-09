import { expect, test, type Route } from "@playwright/test";

// The bug report wizard end to end in the web UI: pick the problem the app recorded, build the
// report (redacted, with the timeline and MISSING.txt), then Post to GitHub / Discord.

const HOST = "192.168.86.100";

test("the wizard builds a redacted report and pre-fills GitHub", async ({ page }) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
    // Record what the page asks the browser to open instead of opening it.
    (window as unknown as { __opened: string[] }).__opened = [];
    window.open = ((url: string) => {
      (window as unknown as { __opened: string[] }).__opened.push(url);
      return null;
    }) as typeof window.open;
  }, HOST);

  const now = Date.now();
  let bundle: { filename: string; entries: { path: string; text?: string }[] } | null = null;
  const json = (route: Route, body: unknown, status = 200) =>
    route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const p = new URL(req.url()).pathname;
      if (p === "/api/version") return json(route, { caps: {}, version: "6.5.2" });
      if (p === "/api/event-journal")
        return json(route, {
          dropped: 0,
          events: [
            { ts: now - 60_000, src: "helper", cat: "helper", level: "error", code: "helper_fatal", console: HOST, msg: "[fatal] signal 11 while serving frame 88" },
            { ts: now - 50_000, src: "engine", cat: "connection", level: "warn", code: "reconnecting", console: HOST, msg: `reconnecting to ${HOST}`, count: 3, last_ts: now - 40_000 },
          ],
        });
      if (p === "/api/ps5/helper-log-ftp") return json(route, { error: "connect: refused" }, 502);
      if (p === "/api/bug-report/bundle" && req.method() === "POST") {
        bundle = req.postDataJSON();
        return route.fulfill({ status: 200, contentType: "application/zip", body: "PK\u0005\u0006" + "\0".repeat(18) });
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/bug-report", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");

  // Step 1
  await expect(main.getByTestId("br-step-1")).toHaveAttribute("aria-current", "step", { timeout: 30_000 });
  await main.getByLabel("What went wrong?").fill("The helper crashed while I had the Hardware screen open.");
  const problems = main.getByTestId("br-problems");
  await expect(problems).toContainText("signal 11");
  await problems.getByText(/signal 11/).click();
  await main.getByRole("button", { name: "Next" }).click();

  // Step 2
  await expect(main.getByTestId("br-step-2")).toHaveAttribute("aria-current", "step");
  await main.getByRole("button", { name: "Next" }).click();

  // Step 3: Everything is the default
  await expect(main.getByTestId("br-step-3")).toHaveAttribute("aria-current", "step");
  await expect(main.getByRole("checkbox", { name: "Everything" })).toBeChecked();
  await main.getByLabel("Time range (until now)").selectOption("6h");
  await main.getByRole("button", { name: "Next" }).click();

  // Step 4: build
  await main.getByRole("button", { name: "Build report" }).click();
  await expect(main.getByTestId("br-saved")).toBeVisible({ timeout: 30_000 });
  expect(bundle).not.toBeNull();
  const b = bundle as unknown as { filename: string; entries: { path: string; text?: string }[] };
  const paths = b.entries.map((e) => e.path);
  expect(paths).toEqual(expect.arrayContaining(["README.txt", "report.md", "report.json", "timeline.txt", "timeline.jsonl", "MISSING.txt"]));
  const timeline = b.entries.find((e) => e.path === "timeline.txt")?.text ?? "";
  expect(timeline).toContain("signal 11");
  expect(timeline).toContain("<ip-1>");
  expect(JSON.stringify(b.entries)).not.toContain(HOST);
  expect(b.entries.find((e) => e.path === "MISSING.txt")?.text).toContain("helper_ftp: connect: refused");
  expect(b.entries.find((e) => e.path === "report.md")?.text).toContain("signal 11");

  // GitHub, then Discord
  await main.getByRole("button", { name: "Post to GitHub" }).click();
  await main.getByRole("button", { name: "Post to Discord" }).click();
  // Discord copies the text first, then opens the channel (a page that lost focus cannot write the clipboard).
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __opened: string[] }).__opened.length))
    .toBe(2);
  const opened = await page.evaluate(() => (window as unknown as { __opened: string[] }).__opened);
  const gh = new URL(opened[0]);
  expect(gh.pathname).toBe("/phantomptr/ps5upload/issues/new");
  expect(gh.searchParams.get("template")).toBe("bug_report.yml");
  expect(gh.searchParams.get("title")).toContain("The helper crashed");
  expect(gh.searchParams.get("what-happened")).toContain("signal 11");
  expect(gh.searchParams.get("platform")).toBe("Browser / self-hosted web UI");
  expect(opened[1]).toBe("https://discord.com/channels/1464735724434624524/1465533832953462794");
});
