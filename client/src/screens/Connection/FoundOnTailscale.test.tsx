import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import { FoundView, type FoundViewProps } from "./FoundOnTailscale";
import { BlockedNotice } from "./TailnetPanel";
import {
  addTailnetConsole,
  rosterMatch,
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
const fat: FoundConsole = {
  ...pro,
  name: "yunpengs-ps5fat",
  tailnet_name: "yunpengs-ps5fat-ps5u",
  tailnet_ip: "100.64.0.11",
  lan_host: "192.168.1.21",
  hosts: ["192.168.1.21"],
  id: "cd".repeat(32),
};

function view(p: Partial<FoundViewProps>) {
  const props: FoundViewProps = {
    found: null,
    profiles: [],
    looking: false,
    busy: null,
    typed: "",
    err: "",
    onLook: () => {},
    onAdd: () => {},
    onConnect: () => {},
    onTyped: () => {},
    ...p,
  };
  return renderToStaticMarkup(<FoundView {...props} />);
}

function answer(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(body), { status }));
}

afterEach(() => vi.unstubAllGlobals());

describe("Found on your Tailscale network", () => {
  it("lists each console with Add, or Connect when it is on the list already", () => {
    const out = view({
      found: { backend: "builtin", can_list: true, consoles: [pro, fat] },
      // The phone has the Fat already (added at home); the Pro is new.
      profiles: [{ id: "p1", name: "Fat", host: "192.168.1.21" }],
    });
    expect(out).toContain("Found on your Tailscale network");
    expect(out).toContain("yunpengs-ps5pro");
    expect(out).toContain("100.64.0.10");
    expect(out).toContain("at home 192.168.1.21");
    expect(out.match(/data-testid="tailnet-found-row"/g)?.length).toBe(2);
    expect(out.match(/data-testid="tailnet-found-add"/g)?.length).toBe(1);
    expect(out.match(/data-testid="tailnet-found-connect"/g)?.length).toBe(1);
    // A typed address can always be added.
    expect(out).toContain('data-testid="tailnet-found-input"');
  });

  it("says why a console does not answer", () => {
    const out = view({
      found: { backend: "builtin", can_list: true, consoles: [{ ...pro, reachable: false, id: null }] },
    });
    expect(out).toContain("lets only listed devices in");
  });

  it("on a phone with only the Tailscale app: type the address, or sign PS5Upload in", () => {
    const out = view({ found: { backend: "system", can_list: false, consoles: [] } });
    expect(out).toContain('data-testid="tailnet-found-no-list"');
    expect(out).toContain('data-testid="tailnet-found-input"');
  });

  it("off Tailscale: says how to get on, and offers no address field", () => {
    const out = view({ found: { backend: "none", can_list: false, consoles: [] } });
    expect(out).toContain('data-testid="tailnet-found-off"');
    expect(out).not.toContain('data-testid="tailnet-found-input"');
  });

  it("nothing found on a tailnet it can read", () => {
    const out = view({ found: { backend: "builtin", can_list: true, consoles: [] } });
    expect(out).toContain('data-testid="tailnet-found-none"');
  });
});

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

  it("finds the app's own entry by the engine's id, or by any address the console is known at", () => {
    const profiles = [
      { id: "a", host: "192.168.1.21" },
      { id: "b", host: "100.64.0.10:9113" },
    ];
    expect(rosterMatch(fat, profiles)?.id).toBe("a");
    expect(rosterMatch(pro, profiles)?.id).toBe("b");
    expect(rosterMatch({ ...pro, listed: { id: "a", name: "x", host: "elsewhere" } }, profiles)?.id).toBe("a");
    expect(rosterMatch({ ...pro, tailnet_ip: "100.64.0.99" }, profiles)).toBeNull();
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
