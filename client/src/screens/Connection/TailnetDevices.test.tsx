import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { DeviceList, deviceKind, deviceStatus, orderDevices, type DeviceRowData } from "./TailnetDevices";

const peer = (name: string, ip: string, online: boolean, path = "idle", latency_ms = -1): DeviceRowData => ({
  name,
  ip,
  online,
  path,
  latency_ms,
});

const peers = [
  peer("pixel-9-ps5u.tail1.ts.net.", "100.64.0.20", true),
  peer("old-laptop", "100.64.0.21", false),
  peer("yunpengs-ps5fat-ps5u", "100.64.0.11", true, "relayed"),
  peer("yunpengs-ps5pro-ps5u", "100.64.0.10", true, "direct", 24.4),
  peer("yunpengs-mbp-ps5u", "100.64.0.22", true),
];

const html = (devices: ReturnType<typeof orderDevices>) =>
  renderToStaticMarkup(<DeviceList devices={devices} refreshing={false} onRefresh={() => {}} onAdd={() => {}} />);

describe("Tailscale device list", () => {
  it("never says idle: online, offline, connected direct or relayed", () => {
    expect(deviceStatus(peer("a", "1", true))).toEqual({ kind: "online", ms: null });
    expect(deviceStatus(peer("a", "1", false))).toEqual({ kind: "offline", ms: null });
    expect(deviceStatus(peer("a", "1", true, "direct", 24.4))).toEqual({ kind: "direct", ms: 24 });
    expect(deviceStatus(peer("a", "1", true, "relayed"))).toEqual({ kind: "relayed", ms: null });
    const out = html(orderDevices(peers));
    expect(out).not.toMatch(/idle/i);
    expect(out).toContain("Connected · direct 24 ms");
    expect(out).toContain("Connected · relayed");
    expect(out).toContain("Online");
    expect(out).toContain("Offline");
  });

  it("an offline device says when it was last seen, when that is known", () => {
    const out = html(orderDevices([{ ...peer("old-laptop", "100.64.0.21", false), last_seen_ms: Date.now() - 3 * 3600_000 }]));
    expect(out).toContain("Offline · last seen");
  });

  it("knows a PS5, a phone and a computer", () => {
    expect(deviceKind("yunpengs-ps5pro-ps5u")).toBe("ps5");
    expect(deviceKind("den-ps5u", true)).toBe("ps5");
    expect(deviceKind("pixel-9-ps5u.tail1.ts.net.")).toBe("phone");
    expect(deviceKind("yuns-iphone")).toBe("phone");
    expect(deviceKind("yunpengs-mbp-ps5u")).toBe("computer");
  });

  it("puts PS5s first, by their name in the app, then this device, then online, then offline", () => {
    const d = orderDevices(peers, {
      self: { name: "yunpengs-mac-ps5u", ip: "100.64.0.2" },
      consoles: [{ host: "192.168.1.20", ip: "100.64.0.10" }],
      profiles: [{ id: "p1", name: "Pro", host: "192.168.1.20" }],
    });
    expect(d.map((x) => x.ip)).toEqual([
      "100.64.0.11",
      "100.64.0.10",
      "100.64.0.2",
      "100.64.0.20",
      "100.64.0.22",
      "100.64.0.21",
    ]);
    // (The two PS5s keep the engine's order among themselves.)
    expect(d[1].rosterName).toBe("Pro");
    expect(d[2].self).toBe(true);
    const out = html(d);
    expect(out).toContain("Pro");
    expect(out).toContain(" · yunpengs-ps5pro-ps5u");
    expect(out).toContain("This device");
    // Five others, four of them online.
    expect(out).toContain("Devices on your Tailscale network (5 · 4 online)");
    // The PS5 the app does not list yet can be added; the listed one cannot.
    expect(out.match(/data-testid="tn-device-add"/g)?.length).toBe(1);
    expect(out).toContain('data-testid="tn-devices-refresh"');
  });

  it("an empty tailnet says so", () => {
    const out = html(orderDevices([], { self: { name: "me", ip: "100.64.0.2" } }));
    expect(out).toContain("No other devices on your Tailscale network yet.");
    expect(out).toContain("(0 · 0 online)");
  });
});
