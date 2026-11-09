import { describe, expect, it } from "vitest";

import { EMPTY_HOST_RUNTIME, type HostRuntime } from "../../../state/connection";
import type { DiscoveredHost } from "../../../api/ps5";
import type { FoundConsole } from "../../../api/tailnet";
import {
  addAddress,
  addTarget,
  cardStatus,
  deviceVia,
  inviteLink,
  isTailnetIp,
  mergeFound,
  parseInvite,
  showChecklist,
  type CardInput,
} from "./model";

const KEY_A = "a".repeat(64);
const KEY_B = "b".repeat(64);

const rt = (patch: Partial<HostRuntime>): HostRuntime => ({ ...EMPTY_HOST_RUNTIME, ...patch });
const input = (patch: Partial<CardInput>): CardInput => ({
  host: "192.168.1.100",
  runtime: EMPTY_HOST_RUNTIME,
  tailnet: null,
  appVersion: "12.1.0",
  stopped: false,
  sending: false,
  ...patch,
});

describe("Consoles › a card at a glance", () => {
  it("ready on the home network: online, home path, helper matches, no checklist", () => {
    const s = cardStatus(
      input({
        runtime: rt({
          payloadStatus: "up",
          payloadVersion: "12.1.0",
          session: "connected",
          power: "awake",
          ps5Kernel: "FreeBSD 11.0 releng/11.0 #0 r218215/releases/09.60: Mon Aug 26 2024",
          ucredElevated: true,
        }),
      }),
    );
    expect(s.power).toBe("online");
    expect(s.path).toEqual({ kind: "home", addr: "192.168.1.100" });
    expect(s.helper).toBe("match");
    expect(s.kernelRw).toBe(true);
    expect(s.ready).toBe(true);
    expect(s.step).toBe("ready");
    expect(showChecklist(s)).toBe(false);
  });

  it("on but without the helper: the checklist shows, at the helper step", () => {
    const s = cardStatus(input({ runtime: rt({ payloadStatus: "down", power: "awake" }) }));
    expect(s.power).toBe("online");
    expect(s.helper).toBe("missing");
    expect(s.step).toBe("helper");
    expect(showChecklist(s)).toBe(true);
  });

  it("not answering at all: offline, no path, the checklist at reaching it", () => {
    const s = cardStatus(input({ runtime: rt({ payloadStatus: "down", power: "unreachable" }) }));
    expect(s.power).toBe("offline");
    expect(s.path.kind).toBe("none");
    expect(s.step).toBe("address");
    expect(showChecklist(s)).toBe(true);
  });

  it("away: reached over Tailscale when home does not answer", () => {
    const s = cardStatus(
      input({
        runtime: rt({ payloadStatus: "up", payloadVersion: "12.1.0", session: "connected" }),
        tailnet: { ip: "100.101.4.7", via: "relayed", latencyMs: 80 },
      }),
    );
    expect(s.path).toEqual({ kind: "tailscale", addr: "100.101.4.7", via: "relayed" });
    expect(s.ready).toBe(true);
  });

  it("in rest mode: calm, no checklist (setup does not fix sleep)", () => {
    const s = cardStatus(input({ runtime: rt({ payloadStatus: "down", power: "rest" }) }));
    expect(s.power).toBe("rest");
    expect(showChecklist(s)).toBe(false);
  });

  it("a helper that answers but has not let this device in is not ready", () => {
    const s = cardStatus(input({ runtime: rt({ payloadStatus: "up", session: "needs_pairing" }) }));
    expect(s.helper).toBe("pairing");
    expect(s.ready).toBe(false);
    expect(s.step).toBe("helper");
  });

  it("an older helper reads update available; sending and stopped say so", () => {
    expect(cardStatus(input({ runtime: rt({ payloadStatus: "up", payloadVersion: "11.9.0" }) })).helper).toBe("update");
    expect(cardStatus(input({ runtime: rt({ payloadStatus: "up", payloadVersion: "13.0.0" }) })).helper).toBe("newer");
    expect(cardStatus(input({ sending: true, runtime: rt({ payloadStatus: "down" }) })).helper).toBe("sending");
    expect(cardStatus(input({ stopped: true, runtime: rt({ payloadStatus: "down", power: "awake" }) })).helper).toBe("stopped");
  });

  it("not probed yet: checking, no checklist", () => {
    const s = cardStatus(input({}));
    expect(s.power).toBe("unknown");
    expect(showChecklist(s)).toBe(false);
  });
});

describe("Consoles › how a device was let in", () => {
  it("reads the helper's word, and the address for an older helper", () => {
    expect(deviceVia({ via: "approved", ip: "192.168.1.5" })).toBe("invite");
    expect(deviceVia({ via: "tailscale", ip: "100.64.0.9" })).toBe("tailscale");
    expect(deviceVia({ via: "network", ip: "192.168.1.5" })).toBe("network");
    expect(deviceVia({ via: "", ip: "192.168.1.5" })).toBe("paired");
    expect(deviceVia({ via: undefined, ip: "100.100.1.1" })).toBe("tailscale");
    expect(deviceVia({ via: undefined, ip: "10.0.0.4" })).toBe("network");
  });

  it("knows Tailscale's range", () => {
    expect(isTailnetIp("100.64.0.1")).toBe(true);
    expect(isTailnetIp("100.127.255.1")).toBe(true);
    expect(isTailnetIp("100.128.0.1")).toBe(false);
    expect(isTailnetIp("100.63.0.1")).toBe(false);
    expect(isTailnetIp("192.168.1.1")).toBe(false);
  });
});

describe("Consoles › invite links", () => {
  it("round-trips name, addresses and key", () => {
    const inv = { name: "Living room", home: "192.168.1.100", tailnet: "100.101.4.7", key: KEY_A };
    const link = inviteLink(inv);
    expect(link.startsWith("ps5upload://console?")).toBe(true);
    expect(parseInvite(`  ${link}\n`)).toEqual(inv);
  });

  it("carries no secret: only n, h, t and k", () => {
    const u = new URL(inviteLink({ name: "PS5", home: "10.0.0.2", tailnet: null, key: KEY_A }));
    expect([...u.searchParams.keys()].sort()).toEqual(["h", "k", "n"]);
  });

  it("refuses what is not an invite, or reaches nothing", () => {
    expect(parseInvite("https://example.com/console?h=1.2.3.4")).toBeNull();
    expect(parseInvite("ps5upload://join?link=x")).toBeNull();
    expect(parseInvite("ps5upload://console?n=PS5")).toBeNull();
    expect(parseInvite("not a link")).toBeNull();
    // A malformed key is dropped, the addresses still count; an address with odd characters is not.
    expect(parseInvite("ps5upload://console?h=10.0.0.2&k=zz")).toEqual({ name: "", home: "10.0.0.2", tailnet: null, key: null });
    expect(parseInvite("ps5upload://console?h=%3Cscript%3E")).toBeNull();
  });
});

const lanHost = (ip: string, patch: Partial<DiscoveredHost> = {}): DiscoveredHost => ({
  ip,
  hostname: null,
  all_ips: [ip],
  services: [],
  loader_port_open: true,
  payload_port_open: true,
  confidence: 80,
  likely_ps5: true,
  ...patch,
});

const tsConsole = (patch: Partial<FoundConsole>): FoundConsole => ({
  name: "living-room",
  tailnet_name: "living-room-ps5u",
  tailnet_ip: "100.101.4.7",
  lan_host: null,
  id: null,
  reachable: true,
  via: "builtin",
  online: true,
  paired: false,
  hosts: [],
  listed: null,
  error: null,
  ...patch,
});

describe("Consoles › Add a PS5 merges what it finds", () => {
  it("a console found at home and on Tailscale is one row with both badges", () => {
    const rows = mergeFound(
      [lanHost("192.168.1.100")],
      [tsConsole({ lan_host: "192.168.1.100", id: KEY_A })],
      null,
      {},
      [],
    );
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      key: KEY_A,
      name: "living-room",
      home: "192.168.1.100",
      tailnet: "100.101.4.7",
      sources: ["home", "tailscale"],
    });
    expect(addAddress(rows[0])).toBe("192.168.1.100");
  });

  it("Add uses the home address found at home, and the Tailscale one for an invite opened away", () => {
    const both = { home: "192.168.1.100", tailnet: "100.101.4.7" };
    // Found on the home network now: home, whatever else is known.
    expect(addTarget({ ...both, sources: ["home", "invite"] }, true)).toBe("192.168.1.100");
    // Only from an invite (or Tailscale), with this device on Tailscale: the Tailscale address,
    // which the engine files under the home address it knows.
    expect(addTarget({ ...both, sources: ["invite"] }, true)).toBe("100.101.4.7");
    expect(addTarget({ ...both, sources: ["tailscale", "invite"] }, true)).toBe("100.101.4.7");
    // Not on Tailscale: Tailscale is no way in, so the home address.
    expect(addTarget({ ...both, sources: ["invite"] }, false)).toBe("192.168.1.100");
    expect(addTarget({ home: null, tailnet: "100.101.4.7", sources: ["invite"] }, false)).toBe("100.101.4.7");
    expect(addTarget({ home: "192.168.1.100", tailnet: null, sources: ["invite"] }, true)).toBe("192.168.1.100");
  });

  it("merges by identity even when no address is shared", () => {
    const rows = mergeFound([lanHost("192.168.1.100")], [tsConsole({ id: KEY_A })], null, { "192.168.1.100": KEY_A }, []);
    expect(rows).toHaveLength(1);
    expect(rows[0].sources).toEqual(["home", "tailscale"]);
  });

  it("an invite joins the console it names, and never duplicates it", () => {
    const rows = mergeFound(
      [lanHost("192.168.1.100")],
      [tsConsole({ id: KEY_A, lan_host: "192.168.1.100" })],
      { name: "Living-room PS5", home: "192.168.1.100", tailnet: "100.101.4.7", key: KEY_A },
      { "192.168.1.100": KEY_A },
      [],
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].sources).toEqual(["home", "tailscale", "invite"]);
    // The name a person gave beats an mDNS or tailnet name.
    expect(rows[0].name).toBe("Living-room PS5");
  });

  it("two consoles stay two, even at one address (two different keys)", () => {
    const rows = mergeFound(
      [lanHost("192.168.1.100"), lanHost("192.168.1.101")],
      [tsConsole({ id: KEY_B, lan_host: "192.168.1.100", tailnet_ip: "100.101.4.8" })],
      null,
      { "192.168.1.100": KEY_A },
      [],
    );
    expect(rows).toHaveLength(3);
    const keyed = rows.filter((r) => r.key).map((r) => r.key).sort();
    expect(keyed).toEqual([KEY_A, KEY_B]);
  });

  it("says which are on the list already, and lists them last", () => {
    const rows = mergeFound(
      [lanHost("192.168.1.100"), lanHost("192.168.1.50", { confidence: 30 })],
      [],
      null,
      {},
      [{ id: "p1", host: "192.168.1.100" }],
    );
    expect(rows.map((r) => [r.home, r.listedId])).toEqual([
      ["192.168.1.50", null],
      ["192.168.1.100", "p1"],
    ]);
  });

  it("a host with several addresses is one row", () => {
    const rows = mergeFound(
      [lanHost("192.168.1.100", { all_ips: ["192.168.1.100", "fe80::1"] })],
      [tsConsole({ hosts: ["fe80::1"] })],
      null,
      {},
      [],
    );
    expect(rows).toHaveLength(1);
  });
});
