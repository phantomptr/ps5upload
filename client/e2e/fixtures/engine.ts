// What most specs start from: a console the app already knows, and an engine that answers JSON.
import type { Page, Request } from "@playwright/test";

/** The made-up paired console (its key built here so it reads as no secret). */
export const LIVING_ROOM = {
  id: "p1",
  name: "Living room",
  host: "192.168.1.100",
  wake_regist_key: "00112233",
  wake_rp_key: "0f".repeat(16),
};

/** Before the page loads: `profile` (Living room unless changed) is the saved, selected console. */
export async function seedConsole(page: Page, profile: Partial<typeof LIVING_ROOM> = {}) {
  const seeded = { ...LIVING_ROOM, ...profile };
  await page.addInitScript((p) => {
    window.localStorage.setItem("ps5upload.host", p.host);
    window.localStorage.setItem("ps5upload.roster.v1", JSON.stringify({ active_id: p.id, profiles: [p] }));
  }, seeded);
}

/**
 * Every `/api/` call answered as JSON with what `answer` returns (its path, the request).
 * `undefined` answers `{}`: a client that only needs the call to succeed gets just that.
 */
export async function routeEngine(
  page: Page,
  answer: (path: string, req: Request) => unknown | Promise<unknown>,
) {
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const body = await answer(new URL(req.url()).pathname, req);
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body === undefined ? {} : body) });
    },
  );
}
