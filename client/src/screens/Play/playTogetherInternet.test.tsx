import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { installWindowStub } from "../../test-utils/storage";
import { NetworkChoice, savedNetwork, saveNetwork } from "./NetworkChoice";
import { netInviteNote, netPeersView, PlayTogetherPanel } from "./PlayTogether";
import { hubUrlOk, joinApi, parseStun, readJoinState } from "./playTogetherState";

// Internet invites (join_net): what the engine says (join.rs state_json, hub.rs Room::json),
// field for field.
const KEY = "0f".repeat(16);
const NET_URL = `https://ps5upload.phantomptr.com/join/#room=r1&k=${KEY}&fp=AA:BB&hub=hub.phantomptr.com`;
const engine = {
  open: true,
  console: "192.168.1.100",
  invite: {
    key: KEY,
    code: "4821",
    url: `http://192.168.1.9:19113/join#i=${KEY}`,
    urls: [`http://192.168.1.9:19113/join#i=${KEY}`],
    expires_in_s: 1800,
    internet: { url: NET_URL, room: "r1", hub: "hub.phantomptr.com", fp: "AA:BB", state: "hosted", error: null, router: null, peers: [] },
  },
  guests: [
    { id: 5, name: "Ann", player: 2, state: "playing", connected: true, rtt_ms: 48, via: "internet", path: "direct", ice_rtt_ms: 40 },
    { id: 6, name: "Ben", player: 3, state: "playing", connected: true, rtt_ms: 9, via: "lan", path: null, ice_rtt_ms: null },
  ],
  pending: [],
  settings: {
    block_system_buttons: true,
    max_players: 3,
    stream_seat: false,
    internet_invites: true,
    hub_url: "wss://hub.phantomptr.com/ws",
    stun: ["stun.cloudflare.com:3478", "stun.l.google.com:19302"],
    upnp: true,
  },
  internet: {
    room: "r1",
    peers: [
      { gid: "g1", path: "direct", ice: "connected", rtt_ms: 40, since_s: 20, why: null },
      { gid: "g2", path: "connecting", ice: "checking", rtt_ms: null, since_s: 2, why: null },
      { gid: "g3", path: "failed", ice: "failed", rtt_ms: null, since_s: 16, why: "no_direct" },
    ],
  },
};

// English, as tr falls back without a language loaded.
const tr = ((_k: string, vars?: Record<string, string | number> | string, fallback?: string) =>
  typeof vars === "string" ? vars : (fallback ?? _k)) as Parameters<typeof netInviteNote>[0];

afterEach(() => vi.unstubAllGlobals());

describe("reading the engine's internet invites", () => {
  it("reads the link, the hub, each guest's way in and the peers", () => {
    const s = readJoinState(engine);
    expect(s.invite?.internet).toEqual({ url: NET_URL, hubState: "hosted", hubError: null });
    expect(s.guests.map((g) => [g.name, g.via, g.path, g.rttMs])).toEqual([
      ["Ann", "internet", "direct", 48],
      ["Ben", "lan", null, 9],
    ]);
    expect(s.netPeers.map((p) => p.path)).toEqual(["direct", "connecting", "failed"]);
    expect(s.settings).toMatchObject({ internetInvites: true, hubUrl: "wss://hub.phantomptr.com/ws", upnp: true });
    expect(s.settings.stun).toHaveLength(2);
  });

  it("an internet guest with no own round trip yet shows ICE's", () => {
    const s = readJoinState({ guests: [{ id: 1, player: 2, via: "internet", path: "direct", rtt_ms: null, ice_rtt_ms: 31 }] });
    expect(s.guests[0].rttMs).toBe(31);
  });

  it("an older engine: no internet choice, every guest on the home network", () => {
    const s = readJoinState({ open: true, settings: { block_system_buttons: true }, guests: [{ id: 1, player: 2 }] });
    expect(s.settings.internetInvites).toBeNull();
    expect(s.guests[0].via).toBe("lan");
    expect(s.netPeers).toEqual([]);
    expect(readJoinState({ invite: { url: "http://x/join#i=k" } }).invite?.internet).toBeNull();
  });
});

describe("the internet invite call", () => {
  function fake(status: number, json: object) {
    const calls: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
        return url.includes("/api/join?")
          ? new Response(JSON.stringify(engine), { status: 200 })
          : new Response(JSON.stringify(json), { status });
      }),
    );
    return calls;
  }

  it("asks with `internet`, then reads the whole state", async () => {
    const calls = fake(200, { key: KEY });
    const r = await joinApi.inviteInternet("192.168.1.100", true);
    expect(calls[0].body).toEqual({ addr: "192.168.1.100", new: true, internet: true });
    expect(r.state?.invite?.internet?.url).toBe(NET_URL);
  });

  it("says why the engine refused (off in the settings, no usable hub)", async () => {
    fake(409, { error: "internet_off" });
    expect(await joinApi.inviteInternet("192.168.1.100")).toEqual({ error: "internet_off" });
    fake(409, { error: "hub_url" });
    expect(await joinApi.inviteInternet("192.168.1.100")).toEqual({ error: "hub_url" });
  });

  it("the home network's invite never asks for internet", async () => {
    const calls = fake(200, { key: KEY });
    await joinApi.invite("192.168.1.100");
    expect(calls[0].body).toEqual({ addr: "192.168.1.100" });
  });

  it("saves the settings and says which one the engine refused", async () => {
    const calls = fake(400, { error: "stun" });
    expect(await joinApi.netSettings("192.168.1.100", { stun: ["x"] })).toEqual({ error: "stun" });
    expect(calls[0].url).toMatch(/\/api\/join\/settings$/);
  });
});

describe("the settings' checks (as the engine's)", () => {
  it("hub: wss://, ws:// only to this computer", () => {
    expect(hubUrlOk("wss://hub.phantomptr.com/ws")).toBe(true);
    expect(hubUrlOk("ws://127.0.0.1:8787/ws")).toBe(true);
    expect(hubUrlOk("ws://localhost:8787/ws")).toBe(true);
    expect(hubUrlOk("ws://[::1]:8787/ws")).toBe(true);
    expect(hubUrlOk("ws://hub.example.com/ws")).toBe(false);
    expect(hubUrlOk("https://hub.example.com/ws")).toBe(false);
    expect(hubUrlOk("hub.example.com")).toBe(false);
  });

  it("STUN: host:port per line, stun: dropped, at most 8", () => {
    expect(parseStun("stun:stun.cloudflare.com:3478\n\n stun.l.google.com:19302 ")).toEqual([
      "stun.cloudflare.com:3478",
      "stun.l.google.com:19302",
    ]);
    expect(parseStun("")).toEqual([]);
    expect(parseStun("stun.example.com")).toBeNull();
    expect(parseStun("bad host:3478")).toBeNull();
    expect(parseStun("a.example:0")).toBeNull();
    expect(parseStun(Array.from({ length: 9 }, (_, i) => `s${i}.example:3478`).join("\n"))).toBeNull();
  });
});

describe("what the host is told", () => {
  it("internet invites off, a bad hub address, other refusals", () => {
    expect(netInviteNote(tr, "internet_off", null)?.id).toBe("pt-net-off");
    expect(netInviteNote(tr, "internet_off", null)?.text).toContain("Settings › Play Together");
    expect(netInviteNote(tr, "hub_url", null)?.id).toBe("pt-net-hub-url");
    expect(netInviteNote(tr, "no_certificate", null)?.text).toContain("(no_certificate)");
  });

  it("the hub unreachable, from the engine's state", () => {
    const down = netInviteNote(tr, null, { url: NET_URL, hubState: "reconnecting", hubError: "connect: refused" });
    expect(down?.id).toBe("pt-net-hub-down");
    expect(down?.text).toContain("(connect: refused)");
    expect(netInviteNote(tr, null, { url: NET_URL, hubState: "connecting", hubError: null })?.tone).toBe("muted");
    expect(netInviteNote(tr, null, { url: NET_URL, hubState: "hosted", hubError: null })).toBeNull();
  });

  it("peers: those connecting, one failure notice until dismissed", () => {
    const peers = readJoinState(engine).netPeers;
    const v = netPeersView(peers, new Set());
    expect(v.connecting.map((p) => p.gid)).toEqual(["g2"]);
    expect(v.failed.map((p) => p.gid)).toEqual(["g3"]);
    expect(netPeersView(peers, new Set(["g3"])).failed).toEqual([]);
  });
});

describe("the network choice", () => {
  it("offers Internet beside this network, with its one-line explanation when picked", () => {
    const props = {
      urls: engine.invite.urls,
      tailscale: null,
      current: engine.invite.url,
      onPick: () => {},
      otherNetwork: "",
      testId: "pt-addresses",
      addressTestId: "pt-address",
    };
    const off = renderToStaticMarkup(<NetworkChoice {...props} internet={{ on: false, onPick: () => {} }} />);
    expect(off).toContain('data-testid="pt-addresses-internet"');
    expect(off).toMatch(/aria-pressed="true"[^>]*data-testid="pt-addresses-home"/);
    expect(off).not.toContain("Works from anywhere");
    const on = renderToStaticMarkup(<NetworkChoice {...props} internet={{ on: true, onPick: () => {} }} />);
    expect(on).toMatch(/aria-pressed="true"[^>]*data-testid="pt-addresses-internet"/);
    expect(on).toContain("Works from anywhere: your friend&#x27;s browser connects straight to this computer.");
    // No internet offered and one address: nothing to choose.
    expect(renderToStaticMarkup(<NetworkChoice {...props} />)).toBe("");
  });

  it("remembers the last choice on this device", () => {
    installWindowStub();
    expect(savedNetwork()).toBe("home");
    saveNetwork("internet");
    expect(savedNetwork()).toBe("internet");
  });
});

describe("the panel", () => {
  const panel = (state = readJoinState(engine)) =>
    renderToStaticMarkup(
      <PlayTogetherPanel addr="192.168.1.100" state={state} onState={() => {}} onClose={() => {}} onPhones={() => {}} />,
    );

  it("Internet picked: the internet link and its QR code, no LAN short code", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    const out = panel();
    expect(out).toContain(`data-testid="pt-url">${NET_URL.replace(/&/g, "&amp;")}<`);
    expect(out).toContain('data-testid="pt-qr"');
    expect(out).toContain('data-testid="pt-addresses-internet-hint"');
    expect(out).not.toContain('data-testid="pt-code"');
  });

  it("This network picked: the LAN link as before", () => {
    installWindowStub({ "ps5upload.pt_network": "home" });
    const out = panel();
    expect(out).toContain(`data-testid="pt-url">${engine.invite.url}<`);
    expect(out).toContain('data-testid="pt-code"');
    expect(out).toContain('data-testid="pt-addresses-internet"');
  });

  it("guests: Internet · Direct with the delay; connecting peers; one dismissible failure", () => {
    installWindowStub();
    const out = panel();
    expect(out).toMatch(/data-testid="pt-player-via" data-path="direct">Internet · Direct</);
    expect(out.match(/pt-player-via/g)).toHaveLength(1);
    expect(out).toContain(">48 ms<");
    expect(out.match(/data-testid="pt-net-peer"/g)).toHaveLength(1);
    expect(out).toContain("Connecting…");
    expect(out.match(/data-testid="pt-net-peer-failed"/g)).toHaveLength(1);
    expect(out).toContain("Couldn&#x27;t connect from their network");
    expect(out).toContain('data-testid="pt-net-peer-failed-dismiss"');
  });

  it("an older engine offers no Internet", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    const old = readJoinState({ ...engine, settings: { block_system_buttons: true, max_players: 3 }, internet: null });
    const out = panel(old);
    expect(out).not.toContain("pt-addresses-internet");
    expect(out).toContain(`data-testid="pt-url">${engine.invite.url}<`);
  });
});
