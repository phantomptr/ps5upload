import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import { MemoryRouter } from "react-router";
import TailnetPanel from "./TailnetPanel";
import { expiryDays, tailnetOp, tailnetStatus, TailnetUnsupportedError } from "../../api/tailnet";

function answer(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(body), { status }));
}

afterEach(() => vi.unstubAllGlobals());

describe("Tailscale on a console", () => {
  it("reads the console's node status", async () => {
    const f = answer(200, { ok: true, enabled: true, state: "running", ip: "100.64.0.5", peers: [] });
    vi.stubGlobal("fetch", f);
    const st = await tailnetStatus("192.0.2.9");
    expect(st.state).toBe("running");
    expect(String(f.mock.calls[0][0])).toContain("/api/ps5/tailnet?addr=192.0.2.9");
  });

  it("sends enable with the console address and the advanced fields", async () => {
    const f = answer(200, { ok: true, enabled: true, state: "needs-login", login_url: "https://login.example/a/1" });
    vi.stubGlobal("fetch", f);
    const st = await tailnetOp("192.0.2.9", "enable", { control_url: "https://hs.example", hostname: "Den" });
    expect(st.login_url).toBe("https://login.example/a/1");
    const init = f.mock.calls[0][1] ?? {};
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      addr: "192.0.2.9",
      op: "enable",
      control_url: "https://hs.example",
      hostname: "Den",
    });
  });

  it("tells an older helper apart from a failure", async () => {
    vi.stubGlobal("fetch", answer(501, { error: "old", reason: "unsupported" }));
    await expect(tailnetStatus("192.0.2.9")).rejects.toBeInstanceOf(TailnetUnsupportedError);
    vi.stubGlobal("fetch", answer(502, { error: "helper said no", reason: "failed" }));
    await expect(tailnetOp("192.0.2.9", "logout")).rejects.toThrow("helper said no");
  });

  it("warns about key expiry only inside the last week", () => {
    const now = 1_800_000_000;
    expect(expiryDays(0, now)).toBeNull();
    expect(expiryDays(now + 30 * 86400, now)).toBeNull();
    expect(expiryDays(now + 3 * 86400 + 60, now)).toBe(3);
    expect(expiryDays(now - 10, now)).toBe(0);
  });

  it("starts with the button that puts the PS5 on Tailscale", () => {
    vi.stubGlobal("fetch", answer(200, {}));
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <TailnetPanel host="192.0.2.9" />
      </MemoryRouter>,
    );
    expect(html).toContain("Put this PS5 on Tailscale");
    expect(html).toContain("Only Remote Play and PS5Upload can reach the PS5 this way");
    // Both steps are spelled out, so the PS5's sign-in and each device's are not confused.
    expect(html).toContain("Here, at home: put this PS5 on your Tailscale network.");
    expect(html).toContain("Settings › Tailscale › Sign in to Tailscale");
  });
});
