import { expect, test, type Locator, type Page } from "@playwright/test";

// The on-screen controller's touchpad is pressed as well as touched: a finger held on it is the
// DualSense's touchpad button (L1 + R1 + touchpad and other combos read it), a finger that
// slides is a swipe only.

const L1 = 256;
const R1 = 512;
const TOUCHPAD = 16384;

async function sideways(page: Page) {
  const pads: Array<{ buttons: number; touch: unknown }> = [];
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          {
            id: "p1",
            name: "Living room",
            host: "192.168.1.100",
            wake_regist_key: "00112233",
            wake_rp_key: "00000000000000000000000000000001",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.7.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; pad?: { buttons: number; touch: unknown } };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.pad) pads.push(msg.pad);
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").tap();
  await expect(page.getByTestId("rp-touch")).toBeVisible();
  return pads;
}

/** A finger (`id`) going down on, moving over, or lifting from `el`. */
async function finger(el: Locator, type: "pointerdown" | "pointermove" | "pointerup", id: number, dx = 0) {
  await el.evaluate(
    (node, [t, pointerId, shift]) => {
      const r = node.getBoundingClientRect();
      node.dispatchEvent(
        new PointerEvent(t as string, {
          pointerId: pointerId as number,
          pointerType: "touch",
          bubbles: true,
          clientX: r.left + r.width / 2 + (shift as number),
          clientY: r.top + r.height / 2,
        }),
      );
    },
    [type, id, dx] as const,
  );
}

test.describe("phone sideways", () => {
  test.use({ viewport: { width: 844, height: 390 }, hasTouch: true, isMobile: true });

  test("L1 + R1 + touchpad press reach the console together; a swipe does not press", async ({ page }) => {
    const pads = await sideways(page);
    const l1 = page.getByRole("button", { name: "L1" });
    const r1 = page.getByRole("button", { name: "R1" });
    const touchpad = page.getByLabel("Touchpad");

    await finger(l1, "pointerdown", 11);
    await finger(r1, "pointerdown", 12);
    await finger(touchpad, "pointerdown", 13);
    await expect
      .poll(() => pads.some((p) => (p.buttons & (L1 | R1 | TOUCHPAD)) === (L1 | R1 | TOUCHPAD)))
      .toBe(true);
    for (const [el, id] of [[touchpad, 13], [r1, 12], [l1, 11]] as const) await finger(el, "pointerup", id);
    await expect.poll(() => pads.at(-1)?.buttons).toBe(0);

    // A swipe: the finger moves on the touchpad, which is not pushed down.
    const before = pads.length;
    await finger(touchpad, "pointerdown", 14);
    await finger(touchpad, "pointermove", 14, 40);
    await expect.poll(() => pads.at(-1)?.touch).not.toBeNull();
    expect(pads.at(-1)!.buttons & TOUCHPAD).toBe(0);
    expect(pads.length).toBeGreaterThan(before);
    await finger(touchpad, "pointerup", 14);
  });
});
