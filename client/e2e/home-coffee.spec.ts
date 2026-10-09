import { expect, test } from "@playwright/test";

// Buy me a coffee sits right under the connection badge on Home, on a wide window and a phone.

for (const [name, width] of [["desktop", 1280], ["phone", 390]] as const)
  test(`Home shows Buy me a coffee under the connection badge (${name})`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(() => {
      window.localStorage.setItem("ps5upload.host", "192.168.86.100");
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({ profiles: [{ id: "a", name: "PS5", host: "192.168.86.100" }], active_id: "a" }),
      );
      (window as unknown as { __opened: string[] }).__opened = [];
      window.open = ((url: string) => {
        (window as unknown as { __opened: string[] }).__opened.push(url);
        return null;
      }) as typeof window.open;
    });
    await page.route(
      (u) => u.pathname.startsWith("/api/"),
      (route) => route.fulfill({ status: 503, contentType: "application/json", body: "{\"error\":\"down\"}" }),
    );
    await page.goto("/", { waitUntil: "domcontentloaded" });
    const main = page.getByRole("main");
    const badge = main.getByTestId("home-connection-badge");
    const coffee = main.getByRole("button", { name: "Buy me a coffee" });
    await expect(coffee).toBeVisible({ timeout: 30_000 });
    const [b, c] = [await badge.boundingBox(), await coffee.boundingBox()];
    expect(c!.y).toBeGreaterThan(b!.y + b!.height - 1);
    await coffee.click();
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __opened: string[] }).__opened))
      .toEqual(["https://ko-fi.com/B0B81S0WUA"]);
  });
