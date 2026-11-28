import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { UsbNetCard, type UsbNetCardProps } from "./UsbNetSettings";
import {
  isIpv4,
  linkLong,
  linkShort,
  usbNetOp,
  usbNetStatus,
  UsbNetUnsupportedError,
  type UsbNetStatus,
} from "../../api/usbNet";

function answer(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(body), { status }));
}

afterEach(() => vi.unstubAllGlobals());

const status = (over: Partial<UsbNetStatus> = {}): UsbNetStatus => ({
  ok: true,
  experimental: true,
  enabled: true,
  state: "ready",
  error: "",
  adapter: { usb_id: "0bda:8156", chip: "RTL8156B", max_mbps: 2500 },
  link_mbps: 2500,
  ip: "192.168.1.77",
  mask: "255.255.255.0",
  gateway: "192.168.1.1",
  source: "dhcp",
  static_ip: "",
  port: 9120,
  minion: true,
  rx_bytes: 3 * 1024 ** 3,
  tx_bytes: 1024 ** 2,
  rx_frames: 10,
  tx_frames: 10,
  errors: 2,
  link_changes: 1,
  uptime_s: 60,
  restarts: 0,
  seen: [],
  ...over,
});

const noop = () => {};
const render = (over: Partial<UsbNetCardProps> = {}) =>
  renderToStaticMarkup(
    <UsbNetCard st={status()} tooOld={false} busy={false} err="" onToggle={noop} onAddress={noop} {...over} />,
  );

describe("api/usbNet", () => {
  it("reads one console's adapter through the engine", async () => {
    const f = answer(200, status({ route: { reachable: true, preferred: true, checked_ms_ago: 1200 } }));
    vi.stubGlobal("fetch", f);
    const st = await usbNetStatus("192.168.1.20");
    expect(st.state).toBe("ready");
    expect(st.route?.preferred).toBe(true);
    expect(String(f.mock.calls[0][0])).toMatch(/\/api\/ps5\/usbnet\?addr=192\.168\.1\.20$/);
  });

  it("posts the op with the console's address", async () => {
    const f = answer(200, status());
    vi.stubGlobal("fetch", f);
    await usbNetOp("192.168.1.20", "address", { ip: "10.0.0.5", mask: "255.0.0.0", gateway: "" });
    expect(String(f.mock.calls[0][0])).toMatch(/\/api\/ps5\/usbnet$/);
    const init = f.mock.calls[0][1] ?? {};
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      addr: "192.168.1.20",
      op: "address",
      ip: "10.0.0.5",
      mask: "255.0.0.0",
      gateway: "",
    });
    await usbNetOp("192.168.1.20", "enable");
    expect(JSON.parse(String(f.mock.calls[1][1]?.body))).toEqual({ addr: "192.168.1.20", op: "enable" });
  });

  it("calls a helper without the method too old", async () => {
    vi.stubGlobal("fetch", answer(501, { error: "unsupported", reason: "unsupported" }));
    await expect(usbNetStatus("192.168.1.20")).rejects.toBeInstanceOf(UsbNetUnsupportedError);
    vi.stubGlobal("fetch", answer(404, {}));
    await expect(usbNetOp("192.168.1.20", "enable")).rejects.toBeInstanceOf(UsbNetUnsupportedError);
    vi.stubGlobal("fetch", answer(200, { ok: false, reason: "unsupported" }));
    await expect(usbNetStatus("192.168.1.20")).rejects.toBeInstanceOf(UsbNetUnsupportedError);
    vi.stubGlobal("fetch", answer(502, { error: "console not answering" }));
    await expect(usbNetStatus("192.168.1.20")).rejects.toThrow("console not answering");
  });

  it("words link speeds and checks addresses", () => {
    expect([2500, 5000, 10000, 1000, 100].map(linkShort)).toEqual(["2.5G", "5G", "10G", "1G", "100M"]);
    expect(linkShort(0)).toBeNull();
    expect(linkShort(undefined)).toBeNull();
    expect(linkLong(2500)).toBe("2.5 Gbit/s");
    expect(linkLong(100)).toBe("100 Mbit/s");
    expect(linkLong(0)).toBeNull();
    expect(isIpv4("192.168.1.50")).toBe(true);
    expect(isIpv4("192.168.1")).toBe(false);
    expect(isIpv4("192.168.1.256")).toBe(false);
    expect(isIpv4("")).toBe(false);
  });
});

describe("Settings › This PS5 › USB Ethernet adapter", () => {
  it("shows a connected adapter: chip, speed, address, data, and that this computer uses it", () => {
    const html = render({ st: status({ route: { reachable: true, preferred: true, checked_ms_ago: 500 } }) });
    expect(html).toContain("USB Ethernet adapter");
    expect(html).toContain("Experimental");
    expect(html).toContain("Connected");
    expect(html).toContain("RTL8156B · 2.5 Gbit/s");
    expect(html).toContain("Address: 192.168.1.77 (DHCP)");
    expect(html).toContain("Received 3.00 GiB");
    expect(html).toContain("Errors 2");
    expect(html).toContain("Transfers from this computer use the adapter");
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("Supported: Realtek RTL8156B (2.5G), RTL8157 (5G), RTL8159 (10G)");
    expect(html).toContain("The console&#x27;s own network keeps working");
    expect(html).toContain("Static address");
  });

  it("says when this computer cannot reach the adapter", () => {
    const html = render({ st: status({ route: { reachable: false, preferred: false, checked_ms_ago: 800 } }) });
    expect(html).toContain("can&#x27;t reach the adapter&#x27;s address; transfers use the console network");
    const checking = render({ st: status({ route: { reachable: false, preferred: false, checked_ms_ago: null } }) });
    expect(checking).toContain("Checking whether this computer can reach the adapter");
  });

  it("puts each state in plain words", () => {
    const cases: Array<[Partial<UsbNetStatus>, string]> = [
      [{ enabled: false, state: "off" }, "Off"],
      [{ state: "none", adapter: null }, "No adapter plugged in"],
      [{ state: "paused" }, "Paused while the PS5 rests"],
      [{ state: "starting" }, "Starting…"],
      [{ state: "no_link", link_mbps: 0 }, "No cable connected"],
      [{ state: "no_address", ip: "" }, "Waiting for an address"],
      [{ state: "failed", error: "bring-up timed out" }, "Couldn&#x27;t start the adapter — unplug it and plug it back in"],
      [{ state: "unsupported" }, "This adapter&#x27;s chip revision isn&#x27;t supported yet"],
    ];
    for (const [over, words] of cases) {
      const html = render({ st: status(over) });
      expect(html).toContain('data-testid="usbnet-state"');
      expect(html).toContain(words);
    }
    expect(render({ st: status({ state: "failed", error: "bring-up timed out" }) })).toContain("Details: bring-up timed out");
  });

  it("leaves out the adapter's figures while off or with nothing plugged in", () => {
    const off = render({ st: status({ enabled: false, state: "off" }) });
    expect(off).toContain('aria-checked="false"');
    expect(off).not.toContain("usbnet-adapter");
    expect(off).not.toContain("usbnet-ip");
    expect(off).not.toContain("Static address");
    const none = render({ st: status({ state: "none", adapter: null, seen: ["046d:c52b"] }) });
    expect(none).not.toContain("usbnet-adapter");
    expect(none).toContain("Other USB devices: 046d:c52b");
  });

  it("names a static address and a link-local one", () => {
    expect(render({ st: status({ source: "static", static_ip: "192.168.1.77" }) })).toContain("Address: 192.168.1.77 (static)");
    expect(render({ st: status({ source: "link-local", ip: "169.254.3.4" }) })).toContain(
      "Address: 169.254.3.4 (automatic, no DHCP server)",
    );
  });

  it("asks for a newer helper when the helper has no USB adapter support", () => {
    const html = render({ st: null, tooOld: true });
    expect(html).toContain("Update the PS5 helper to use a USB adapter.");
    expect(html).not.toContain('role="switch"');
  });
});
