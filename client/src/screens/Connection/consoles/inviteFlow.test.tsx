// The invite → ask → allow flow against the engine routes it uses, with a fake engine: the
// inviting app's sheet, the console's mesh answer with a device asking, Allow posted to
// /api/mesh/device, and the new device's pairing dialog that waits and then gets in.

import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { meshDevice, toMeshState } from "../../../api/mesh";
import { consoleIdentity } from "../../../api/ava1";
import { usePairingStore } from "../../../state/pairing";
import { appLinkRoute } from "../../../lib/appLinks";
import { AskPrompt, onConsolesPage, pendingAsks } from "./ConsoleAsks";
import { InviteView } from "./InviteSheet";
import { AddSheetView, type ScanState } from "./AddConsoleSheet";
import { inviteLink, mergeFound, parseInvite } from "./model";

const KEY = "ab".repeat(32);
const ASKER = "cd".repeat(32);

/** A fake engine: each route answers from `routes`, and every request is kept. */
function fakeEngine(routes: Record<string, (body: unknown) => unknown>) {
  const calls: { path: string; body: unknown }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const u = new URL(url, "http://engine");
      const body = init?.body ? JSON.parse(String(init.body)) : null;
      calls.push({ path: u.pathname + u.search, body });
      const route = routes[u.pathname];
      if (!route) return new Response("{}", { status: 404 });
      const out = route(body);
      return new Response(JSON.stringify(out), { status: out && (out as { status?: number }).status === 502 ? 502 : 200 });
    }),
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
  usePairingStore.setState({ open: false, host: "", view: null, busy: false, error: null });
});

const meshWithAsk = {
  supported: true,
  rev: 9,
  now: 1000,
  you: "me",
  helper: "12.2.0",
  settings: { require_approval: true, prune_days: 90 },
  asks: [{ id: ASKER, name: "Pixel 9", ip: "192.168.1.30", at: 900 }],
  devices: [{ id: "me", name: "MacBook", nick: "", kind: "desktop", os: "", app: "12.2.0", first: 1, last: 2, ip: "192.168.1.20", online: true, blocked: false, activity: [], via: "network" }],
  locks: [],
  events: [],
  ev: 3,
};

describe("Consoles › invite, ask, allow", () => {
  it("the invite carries the console's key from GET /api/ava1/console, and its link opens Consoles", async () => {
    const calls = fakeEngine({ "/api/ava1/console": () => ({ key: KEY, name: "PS5" }) });
    const id = await consoleIdentity("192.168.1.100");
    expect(id).toEqual({ key: KEY, name: "PS5" });
    expect(calls[0].path).toBe("/api/ava1/console?addr=192.168.1.100");
    const link = inviteLink({ name: "Living room", home: "192.168.1.100", tailnet: "100.101.4.7", key: id!.key });
    expect(appLinkRoute(link)).toBe(`/connection?invite=${encodeURIComponent(link)}`);
    expect(parseInvite(link)?.key).toBe(KEY);
  });

  it("an engine without the route, or a console that does not answer, gives no key", async () => {
    fakeEngine({ "/api/ava1/console": () => ({ status: 502, error: "timed out" }) });
    expect(await consoleIdentity("192.168.1.100")).toBeNull();
    fakeEngine({});
    expect(await consoleIdentity("192.168.1.100")).toBeNull();
  });

  it("the console's answer lists the device asking, and how each device got in", () => {
    const st = toMeshState(meshWithAsk);
    if (!st.supported || st.same) throw new Error("expected a picture");
    expect(st.asks).toEqual([{ id: ASKER, name: "Pixel 9", ip: "192.168.1.30", at: 900 }]);
    expect(st.devices[0].via).toBe("network");
    // An older helper says nothing about asks or via.
    const old = toMeshState({ ...meshWithAsk, asks: undefined, devices: [{ ...meshWithAsk.devices[0], via: undefined }] });
    if (!old.supported || old.same) throw new Error("expected a picture");
    expect(old.asks).toEqual([]);
    expect(old.devices[0].via).toBeUndefined();
  });

  it("the prompt names the device and the console, over every screen", () => {
    const st = toMeshState(meshWithAsk);
    if (!st.supported || st.same) throw new Error("expected a picture");
    const pending = pendingAsks({ "192.168.1.100": { state: st } }, [{ host: "192.168.1.100", name: "Living room" }], new Set());
    expect(pending).toHaveLength(1);
    const html = renderToStaticMarkup(
      <AskPrompt pending={pending[0]} more={0} busy={false} error={null} onAnswer={() => {}} onLater={() => {}} />,
    );
    expect(html).toContain("Pixel 9 wants to use Living room");
    expect(html).toContain("192.168.1.30");
    expect(html).toContain("Allow");
    expect(html).toContain("Decline");
    // Later hides it in this window; a console not on the list is never asked about here.
    expect(pendingAsks({ "192.168.1.100": { state: st } }, [{ host: "192.168.1.100", name: "x" }], new Set([`192.168.1.100:${ASKER}`]))).toEqual([]);
    expect(pendingAsks({ "10.0.0.9": { state: st } }, [{ host: "192.168.1.100", name: "x" }], new Set())).toEqual([]);
  });

  it("the prompt never shows an ask from a stale picture, and stands aside on the Consoles page", () => {
    const st = toMeshState(meshWithAsk);
    if (!st.supported || st.same) throw new Error("expected a picture");
    const profiles = [{ host: "192.168.1.100", name: "Living room" }];
    // The last read failed, or the console's helper is not answering: its old asks may be answered.
    expect(pendingAsks({ "192.168.1.100": { state: st, error: "timeout" } }, profiles, new Set())).toEqual([]);
    expect(pendingAsks({ "192.168.1.100": { state: st } }, profiles, new Set(), () => false)).toEqual([]);
    expect(pendingAsks({ "192.168.1.100": { state: st } }, profiles, new Set(), () => true)).toHaveLength(1);
    // Each card answers its own asks on the Consoles page; Devices and Health Check are other tabs.
    expect(onConsolesPage("/connection", "")).toBe(true);
    expect(onConsolesPage("/connection", "?console=p1")).toBe(true);
    expect(onConsolesPage("/connection", "?tab=devices")).toBe(false);
    expect(onConsolesPage("/upload", "")).toBe(false);
  });

  it("Allow and Decline go to the console's mesh as device ops", async () => {
    const calls = fakeEngine({ "/api/mesh/device": () => ({ supported: true, ok: true, rev: 10 }) });
    expect(await meshDevice("192.168.1.100", "allow", ASKER)).toBe(true);
    expect(await meshDevice("192.168.1.100", "decline", ASKER)).toBe(true);
    expect(calls.map((c) => c.body)).toEqual([
      { addr: "192.168.1.100", op: "allow", id: ASKER },
      { addr: "192.168.1.100", op: "decline", id: ASKER },
    ]);
  });

  it("the new device's dialog waits while the console asks, and gets in once allowed", async () => {
    let allowed = false;
    const calls = fakeEngine({
      "/api/ava1/pairing/start": () => ({ state: allowed ? "accepted" : "closed" }),
    });
    const store = usePairingStore.getState();
    await store.openFor("192.168.1.100");
    expect(usePairingStore.getState().view).toEqual({ state: "closed" });
    let ticks = 0;
    const sleep = async () => {
      ticks++;
      if (ticks === 3) allowed = true; // someone pressed Allow
    };
    await usePairingStore.getState().waitForApproval(sleep);
    expect(usePairingStore.getState().open).toBe(false);
    expect(usePairingStore.getState().paired).toBe("192.168.1.100");
    expect(calls.filter((c) => c.path === "/api/ava1/pairing/start")).toHaveLength(4);
  });

  it("one failed read does not end the wait for an Allow", async () => {
    let n = 0;
    fakeEngine({
      "/api/ava1/pairing/start": () => {
        n++;
        // The second read fails (a busy console); the fourth finds the device allowed.
        if (n === 2) return { status: 502, error: "timeout" };
        return { state: n >= 4 ? "accepted" : "closed" };
      },
    });
    await usePairingStore.getState().openFor("192.168.1.100");
    await usePairingStore.getState().waitForApproval(async () => {});
    expect(usePairingStore.getState().paired).toBe("192.168.1.100");
    expect(usePairingStore.getState().error).toBeNull();
  });

  it("stops waiting when the dialog is closed", async () => {
    fakeEngine({ "/api/ava1/pairing/start": () => ({ state: "closed" }) });
    await usePairingStore.getState().openFor("192.168.1.100");
    const sleep = async () => {
      usePairingStore.getState().close();
    };
    await usePairingStore.getState().waitForApproval(sleep);
    expect(usePairingStore.getState().open).toBe(false);
  });

  it("the invite sheet shows the code, the key, and the device asking with Allow", () => {
    const st = toMeshState(meshWithAsk);
    if (!st.supported || st.same) throw new Error("expected a picture");
    const html = renderToStaticMarkup(
      <InviteView
        invite={{ name: "Living room", home: "192.168.1.100", tailnet: "100.101.4.7", key: KEY }}
        loading={false}
        state={st}
        busy={false}
        error={null}
        copied={false}
        onCopy={() => {}}
        onAskFirst={() => {}}
        onAnswer={() => {}}
      />,
    );
    expect(html).toContain('data-testid="consoles-invite-qr"');
    // The console's key rides in the link only: never shown to a person as raw hex.
    expect(html).not.toContain("abab abab");
    expect(html).not.toContain("Console key");
    expect(html).toContain("ps5upload://console?");
    expect(html).toContain("lets nobody in by itself");
    expect(html).toContain("Pixel 9 wants to use Living room");
  });

  it("the Add sheet shows a read invite merged with what the scans found: one row", () => {
    const invite = { name: "Living room", home: "192.168.1.100", tailnet: "100.101.4.7", key: KEY };
    const scan: ScanState = {
      scanning: false,
      lan: [
        { ip: "192.168.1.100", hostname: "PS5-123", all_ips: ["192.168.1.100"], services: [], loader_port_open: true, payload_port_open: true, confidence: 90, likely_ps5: true },
      ],
      lanError: null,
      lanDone: true,
      tailnet: {
        backend: "builtin",
        can_list: true,
        consoles: [
          { name: "living-room", tailnet_name: "living-room-ps5u", tailnet_ip: "100.101.4.7", lan_host: null, id: KEY, reachable: true, via: "builtin", online: true, paired: false, hosts: [], listed: null, error: null },
        ],
      },
      tailnetError: null,
      lanKeys: { "192.168.1.100": KEY },
    };
    const rows = mergeFound(scan.lan, scan.tailnet!.consoles, invite, scan.lanKeys, []);
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <AddSheetView
          scan={scan}
          invite={invite}
          rows={rows}
          busy={null}
          typed=""
          pasted=""
          scanningQr={false}
          qrOk={false}
          error={null}
          firstRun={false}
          onScan={() => {}}
          onAdd={() => {}}
          onOpen={() => {}}
          onTyped={() => {}}
          onAddTyped={() => {}}
          onPasted={() => {}}
          onScanQr={() => {}}
          onQrInvite={() => {}}
          onQrClose={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html.match(/data-testid="consoles-found-row"/g)).toHaveLength(1);
    expect(html).toContain("Home network");
    expect(html).toContain("Tailscale");
    expect(html).toContain("Invite");
    // No camera here: paste only.
    expect(html).not.toContain('data-testid="consoles-scan-qr"');
    expect(html).toContain('data-testid="consoles-invite-paste"');
  });
});
