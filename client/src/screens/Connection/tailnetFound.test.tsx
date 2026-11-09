import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import { BlockedNotice } from "./TailnetPanel";
import {
  addTailnetConsole,
  tailnetConsoles,
  type FoundConsole,
} from "../../api/tailnet";

const pro: FoundConsole = {
  name: "yunpengs-ps5pro",
  tailnet_name: "yunpengs-ps5pro-ps5u",
  tailnet_ip: "100.64.0.10",
  lan_host: null,
  id: "ab".repeat(32),
  reachable: true,
  via: "builtin",
  online: true,
  paired: false,
  hosts: [],
  listed: null,
  error: null,
};

function answer(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(body), { status }));
}

afterEach(() => vi.unstubAllGlobals());

describe("the Add flow", () => {
  it("asks the engine for the list, fresh on Look again", async () => {
    const f = answer(200, { ok: true, backend: "builtin", can_list: true, consoles: [pro] });
    vi.stubGlobal("fetch", f);
    const got = await tailnetConsoles();
    expect(got.consoles[0].tailnet_ip).toBe("100.64.0.10");
    expect(String(f.mock.calls[0][0])).toMatch(/\/api\/tailnet\/consoles$/);
    await tailnetConsoles(true);
    expect(String(f.mock.calls[1][0])).toContain("/api/tailnet/consoles?refresh=1");
  });

  it("an engine without the route reads as nothing found", async () => {
    vi.stubGlobal("fetch", answer(404, {}));
    expect(await tailnetConsoles()).toEqual({ backend: "none", can_list: false, consoles: [] });
  });

  it("adds through the engine, which keeps one entry per console", async () => {
    const f = answer(200, {
      ok: true,
      action: "added",
      console: { id: "new-1", name: "yunpengs-ps5pro", host: "100.64.0.10" },
      found: pro,
    });
    vi.stubGlobal("fetch", f);
    const r = await addTailnetConsole(" 100.64.0.10 ", "yunpengs-ps5pro");
    expect(r.action).toBe("added");
    expect(r.console.host).toBe("100.64.0.10");
    const init = f.mock.calls[0][1] ?? {};
    expect(String(f.mock.calls[0][0])).toContain("/api/tailnet/consoles/add");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({ addr: "100.64.0.10", name: "yunpengs-ps5pro" });
  });

  it("a typed address that is not a Tailscale one is refused in words", async () => {
    vi.stubGlobal(
      "fetch",
      answer(400, { error: "not a Tailscale address: type the PS5's 100.x address", reason: "not_tailnet" }),
    );
    await expect(addTailnetConsole("192.168.1.5")).rejects.toThrow(/not a Tailscale address/);
  });

});

describe("a device the PS5's list turned away", () => {
  it("says who tried, with Allow and Ignore", () => {
    const out = renderToStaticMarkup(
      <BlockedNotice
        blocked={[
          { ip: "100.64.0.7", name: "pixel-9.tail1.ts.net.", port: 9120, proto: "tcp", count: 3, first: 1, last: 2 },
          { ip: "100.64.0.8", name: "", port: 9295, proto: "tcp", count: 1, first: 1, last: 1 },
        ]}
        busy={false}
        onAllow={() => {}}
        onIgnore={() => {}}
      />,
    );
    expect(out).toContain("pixel-9 tried to reach this PS5 over Tailscale.");
    expect(out).toContain("100.64.0.8 tried to reach this PS5 over Tailscale.");
    expect(out.match(/data-testid="tailnet-blocked-allow"/g)?.length).toBe(2);
    expect(out.match(/data-testid="tailnet-blocked-ignore"/g)?.length).toBe(2);
  });

  it("shows nothing when no one was turned away", () => {
    const out = renderToStaticMarkup(<BlockedNotice blocked={[]} busy={false} onAllow={() => {}} onIgnore={() => {}} />);
    expect(out).toBe("");
  });
});
