import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import { MemoryRouter } from "react-router";
import { TailscaleSettings } from "./TailscaleSettings";
import { appTailnetOp, appTailnetStatus, tailnetProblems, TAILNET_SLOW_MS, tailnetVia } from "../../api/tailnet";
import { TailnetProblems } from "../Connection/TailnetProblems";

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
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <TailscaleSettings />
      </MemoryRouter>,
    );
    expect(html).toContain("Sign in to Tailscale");
    expect(html).toContain("No Tailscale app needed");
    // It says the PS5 joins on its own screen, and leads there.
    expect(html).toContain("press Set up next to Tailscale on the PS5");
    expect(html).toContain("Open Connection");
  });

  it("says what is wrong in words instead of spinning on Connecting", () => {
    const keys = (st: Parameters<typeof tailnetProblems>[0], ms = 0) => tailnetProblems(st, ms).map((p) => p.key);
    // a refused sign-in: the server's own words, or plain ones
    expect(tailnetProblems({ state: "backoff", error: "invalid key: unable to validate API key" }, 0)).toEqual([
      { key: "tn_err_refused_why", fallback: "Tailscale refused the sign-in: {why}", vars: { why: "invalid key: unable to validate API key" }, bad: true },
    ]);
    expect(keys({ state: "backoff", error: "control refused registration" })).toEqual(["tn_err_refused"]);
    // out of reach, signed out
    expect(keys({ state: "backoff", error: "connection error" })).toEqual(["tn_err_unreachable"]);
    expect(keys({ state: "connecting", error: "DNS lookup failed" })).toEqual(["tn_err_unreachable"]);
    expect(keys({ state: "signed-out", error: "signed out at control" })).toEqual(["tn_err_signed_out"]);
    // the key: expired, or expiring beyond what key_expiry says in days
    expect(keys({ state: "running", warning: "this device's Tailscale key has expired; sign in again" })).toEqual(["tn_warn_expired"]);
    expect(keys({ state: "running", warning: "this device's Tailscale key expires in 5 hours" })).toEqual(["tn_warn_expiring"]);
    const soon = Date.now() / 1000 + 3 * 86400;
    expect(keys({ state: "running", warning: "expires in 3 days", key_expiry: soon })).toEqual([]);
    // an old error once running says nothing
    expect(keys({ state: "running", error: "connection error" })).toEqual([]);
    // a sign-in that stays pending
    expect(keys({ state: "needs-login" }, TAILNET_SLOW_MS - 1)).toEqual([]);
    expect(keys({ state: "needs-login" }, TAILNET_SLOW_MS)).toEqual(["tn_slow"]);
    expect(keys({ state: "connecting" }, TAILNET_SLOW_MS * 2)).toEqual(["tn_slow"]);
    expect(keys({ state: "connecting", error: "timeout" }, TAILNET_SLOW_MS)).toEqual(["tn_err_unreachable"]);
  });

  it("shows the problem on the page, and nothing when all is well", () => {
    const html = renderToStaticMarkup(<TailnetProblems st={{ state: "backoff", error: "control refused registration" }} testId="p" />);
    expect(html).toContain('data-testid="p"');
    expect(html).toContain("Tailscale refused the sign-in.");
    expect(renderToStaticMarkup(<TailnetProblems st={{ state: "running" }} testId="p" />)).toBe("");
  });
});
