import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { EMPTY_HOST_RUNTIME, type HostRuntime } from "../../../state/connection";
import type { MeshState } from "../../../api/mesh";
import { ConsoleCardView, tailnetSummary, type ConsoleCardViewProps, type TailnetSummary } from "./ConsoleCard";
import { cardStatus } from "./model";
import { TrustedDevicesView } from "./TrustedDevices";

const profile = { id: "p1", name: "Living room", host: "192.168.1.100" };
const noop = () => {};
const TN_OFF: TailnetSummary = {
  known: true,
  setUp: false,
  on: false,
  connecting: false,
  ip: null,
  testedAt: 0,
  allowMode: null,
  listed: 0,
  inUse: false,
};

function card(runtime: Partial<HostRuntime>, patch: Partial<ConsoleCardViewProps> = {}, tailnet: Parameters<typeof cardStatus>[0]["tailnet"] = null) {
  const status = cardStatus({
    host: profile.host,
    runtime: { ...EMPTY_HOST_RUNTIME, ...runtime },
    tailnet,
    appVersion: "12.1.0",
    stopped: false,
    sending: false,
  });
  const props: ConsoleCardViewProps = {
    profile,
    active: true,
    status,
    tn: TN_OFF,
    mesh: null,
    canWake: true,
    wakeBusy: false,
    wakeError: null,
    tsBusy: false,
    tsOpen: false,
    devicesOpen: false,
    error: null,
    checklist: <div data-testid="checklist-slot" />,
    tailnetPanel: <div data-testid="tailnet-panel-slot" />,
    trusted: <div data-testid="trusted-slot" />,
    stop: <span data-testid="stop-slot" />,
    onUse: noop,
    onResend: noop,
    onWake: noop,
    onSettings: noop,
    onInvite: noop,
    onRename: noop,
    onNotes: noop,
    onRemove: noop,
    onSaveHome: noop,
    onTsSwitch: noop,
    onTsTest: noop,
    onToggleTs: noop,
    onToggleDevices: noop,
    onAnswer: noop,
    ...patch,
  };
  return renderToStaticMarkup(
    <MemoryRouter>
      <ConsoleCardView {...props} />
    </MemoryRouter>,
  );
}

const READY = {
  payloadStatus: "up" as const,
  payloadVersion: "12.1.0",
  session: "connected" as const,
  power: "awake" as const,
  ucredElevated: true,
  ps5Kernel: "FreeBSD 11.0 releng/11.0 #0 r218215/releases/09.60: Mon Aug 26 2024",
};

describe("Consoles › a console's card", () => {
  it("ready: status at a glance, the three ways to reach it, Invite, and no checklist", () => {
    const html = card(READY);
    expect(html).toContain('data-ready="1"');
    expect(html).toContain("Online · Home network 192.168.1.100");
    expect(html).toContain("Helper v12.1.0 · matches this app");
    expect(html).toContain("Kernel access");
    expect(html).toContain("Home network");
    expect(html).toContain("Always tried first.");
    expect(html).toContain("Tailscale");
    expect(html).toContain("Not set up yet. Coming in a later version");
    expect(html).toContain("Invite a device");
    expect(html).toContain("Resend helper");
    expect(html).toContain('data-testid="stop-slot"');
    expect(html).toContain("Settings for this PS5");
    expect(html).not.toContain('data-testid="checklist-slot"');
    // Wake is for a console that is asleep or away, not one that answers.
    expect(html).not.toContain('data-testid="consoles-wake"');
  });

  it("needs the helper: the checklist shows in the card, and no Invite yet", () => {
    const html = card({ payloadStatus: "down", power: "awake" });
    expect(html).toContain('data-ready="0"');
    expect(html).toContain("Helper not running");
    expect(html).toContain('data-testid="checklist-slot"');
    expect(html).not.toContain("Invite a device");
  });

  it("away on Tailscale: says so, and the Tailscale row is in use and tested", () => {
    const tn: TailnetSummary = {
      ...TN_OFF,
      setUp: true,
      on: true,
      ip: "100.101.4.7",
      testedAt: 1_700_000_000_000,
      allowMode: "listed",
      listed: 2,
      inUse: true,
    };
    const html = card(READY, { tn }, { ip: "100.101.4.7", via: "relayed", latencyMs: 80 });
    expect(html).toContain("Online · Tailscale 100.101.4.7, relayed");
    expect(html).toContain('data-testid="consoles-ts-tested"');
    expect(html).toContain("2 devices you ticked");
    expect(html).toContain('data-testid="consoles-ts-switch"');
  });

  it("in rest mode: calm words, Wake, no checklist", () => {
    const html = card({ payloadStatus: "down", power: "rest" });
    expect(html).toContain("In rest mode");
    expect(html).toContain('data-testid="consoles-wake"');
    expect(html).not.toContain('data-testid="checklist-slot"');
  });

  it("a device asking shows Allow / Decline on the card", () => {
    const mesh = {
      rev: 1,
      now: 0,
      you: "me",
      helper: "12.1.0",
      settings: { require_approval: true, prune_days: 90 },
      asks: [{ id: "c".repeat(64), name: "Pixel", ip: "192.168.1.30", at: 1 }],
      devices: [],
      locks: [],
      events: [],
      ev: 0,
    } satisfies MeshState;
    const html = card(READY, { mesh });
    expect(html).toContain("Pixel wants to use Living room");
    expect(html).toContain("Allow");
    expect(html).toContain("Decline");
  });

  it("the Tailscale summary reads the console's own status and this device's", () => {
    const tn = tailnetSummary(
      {
        enabled: true,
        state: "running",
        ip: "100.101.4.7",
        allow_mode: "all",
        allow: "",
      } as never,
      { consoles: [{ host: "192.168.1.100", ip: "100.101.4.7", name: "x", via: "direct", latency_ms: 4, tested_at: 5 }] } as never,
      "192.168.1.100",
    );
    expect(tn).toMatchObject({ setUp: true, on: true, ip: "100.101.4.7", testedAt: 5, allowMode: "all", inUse: true });
  });
});

describe("Consoles › devices allowed to use a console", () => {
  const state = {
    rev: 1,
    now: 0,
    you: "me",
    helper: "12.1.0",
    settings: { require_approval: true, prune_days: 90 },
    asks: [],
    devices: [
      { id: "me", name: "MacBook", nick: "", kind: "desktop", os: "", app: "12.1.0", first: 0, last: 0, ip: "192.168.1.20", online: true, blocked: false, activity: [], via: "" },
      { id: "p", name: "Pixel", nick: "", kind: "android", os: "", app: "12.1.0", first: 0, last: 0, ip: "192.168.1.30", online: false, blocked: false, activity: [], via: "approved" },
      { id: "t", name: "Laptop", nick: "", kind: "desktop", os: "", app: "12.1.0", first: 0, last: 0, ip: "100.64.0.9", online: false, blocked: false, activity: [], via: "tailscale" },
      { id: "x", name: "Blocked one", nick: "", kind: "desktop", os: "", app: "", first: 0, last: 0, ip: "1.2.3.4", online: false, blocked: true, activity: [], via: "network" },
    ],
    locks: [],
    events: [],
    ev: 0,
  } satisfies MeshState;

  it("lists each with how it was let in, Revoke for the others, and the ask-first switch", () => {
    const html = renderToStaticMarkup(
      <TrustedDevicesView
        state={state}
        consoleName="Living room"
        busy={false}
        error={null}
        onAnswer={noop}
        onRevoke={noop}
        onBlock={noop}
        onAskFirst={noop}
      />,
    );
    expect(html).toContain("MacBook");
    expect(html).toContain("This device");
    expect(html).toContain("Invited, and approved");
    expect(html).toContain("Let in over Tailscale");
    expect(html).toContain("Paired with a code, or sent the helper");
    expect(html).not.toContain("Blocked one");
    // Revoke for Pixel and Laptop, never for this device.
    expect(html.match(/Revoke/g)).toHaveLength(2);
    expect(html).toContain("Ask before a new device can connect");
  });

  it("says where the list lives while the helper is away", () => {
    const html = renderToStaticMarkup(
      <TrustedDevicesView state={null} consoleName="x" busy={false} error={null} onAnswer={noop} onRevoke={noop} onBlock={noop} onAskFirst={noop} />,
    );
    expect(html).toContain("kept on the PS5");
  });
});
