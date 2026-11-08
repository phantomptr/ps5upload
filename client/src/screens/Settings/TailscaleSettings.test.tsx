import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import { TailscaleSettings } from "./TailscaleSettings";
import { appTailnetOp, appTailnetStatus, tailnetVia } from "../../api/tailnet";

function answer(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(body), { status }));
}

afterEach(() => vi.unstubAllGlobals());

describe("Settings › Tailscale (this device's own node)", () => {
  it("reads and changes the engine's node", async () => {
    const f = answer(200, { ok: true, enabled: true, state: "needs-login", login_url: "https://login.example/a/2" });
    vi.stubGlobal("fetch", f);
    expect((await appTailnetStatus()).state).toBe("needs-login");
    expect(String(f.mock.calls[0][0])).toMatch(/\/api\/tailnet$/);
    await appTailnetOp("enable", { control_url: "https://hs.example", hostname: "Laptop" });
    const init = f.mock.calls[1][1] ?? {};
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({ op: "enable", control_url: "https://hs.example", hostname: "Laptop" });
  });

  it("says how a console is reached, and treats any failure as the LAN", async () => {
    vi.stubGlobal("fetch", answer(200, { via: "relayed", latency_ms: -1 }));
    expect((await tailnetVia("192.0.2.9")).via).toBe("relayed");
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new Error("engine down"))));
    expect((await tailnetVia("192.0.2.9")).via).toBe("lan");
  });

  it("starts with sign-in and says no Tailscale app is needed", () => {
    vi.stubGlobal("fetch", answer(200, {}));
    const html = renderToStaticMarkup(<TailscaleSettings />);
    expect(html).toContain("Sign in to Tailscale");
    expect(html).toContain("no Tailscale app needed");
  });
});
