import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import en from "../locales/en.json";
import { usbNetAdapterLine, usbNetLine, UsbNetSectionView, type ConsoleUsbNet } from "./UsbNet";

const ready: ConsoleUsbNet = {
  enabled: true,
  state: "ready",
  adapter: { usb_id: "0bda:8157", chip: "RTL8157", max_mbps: 5000 },
  link_mbps: 5000,
  ip: "192.168.1.77",
  source: "dhcp",
};

const noop = () => {};

describe("console page › USB Ethernet adapter", () => {
  it("says where it stands in words", () => {
    expect(usbNetLine(ready)).toBe("Connected");
    expect(usbNetLine({ enabled: false, state: "off" })).toBe("Off");
    expect(usbNetLine({ enabled: true, state: "none" })).toBe("No adapter plugged in");
    expect(usbNetLine({ enabled: true, state: "paused" })).toBe("Paused while the PS5 rests");
    expect(usbNetLine({ enabled: true, state: "no_link" })).toBe("No cable connected");
    expect(usbNetLine({ enabled: true, state: "no_address" })).toBe("Waiting for an address");
    expect(usbNetLine({ enabled: true, state: "failed" })).toContain("unplug it and plug it back in");
    expect(usbNetLine({ enabled: true, state: "unsupported" })).toContain("isn't supported yet");
    // A state this page does not know yet reads as starting.
    expect(usbNetLine({ enabled: true, state: "something-new" })).toBe("Starting…");
  });

  it("names the chip and speed only while an adapter is in use", () => {
    expect(usbNetAdapterLine(ready)).toBe("RTL8157 · 5 Gbit/s");
    expect(usbNetAdapterLine({ ...ready, link_mbps: 0 })).toBe("RTL8157");
    expect(usbNetAdapterLine({ ...ready, enabled: false, state: "off" })).toBeNull();
    expect(usbNetAdapterLine({ enabled: true, state: "none", adapter: null })).toBeNull();
  });

  it("renders the switch, state, chip, address and the note", () => {
    const html = renderToStaticMarkup(<UsbNetSectionView st={ready} busy={false} err={null} onToggle={noop} />);
    expect(html).toContain('data-setting="usbnet"');
    expect(html).toContain(en.cw_usbnet_title);
    expect(html).toContain(en.cw_usbnet_experimental);
    expect(html).toContain('role="switch"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("Connected");
    expect(html).toContain("RTL8157 · 5 Gbit/s");
    expect(html).toContain("Address: 192.168.1.77");
    expect(html).toContain("own network keeps working.");
  });

  it("shows a failure as an alert and a change error", () => {
    const html = renderToStaticMarkup(
      <UsbNetSectionView st={{ ...ready, state: "failed" }} busy={false} err="Couldn't change" onToggle={noop} />,
    );
    expect(html).toContain('role="alert"');
    expect(html).toContain("Couldn&#x27;t change");
    const off = renderToStaticMarkup(
      <UsbNetSectionView st={{ enabled: false, state: "off" }} busy={false} err={null} onToggle={noop} />,
    );
    expect(off).toContain('aria-checked="false"');
    expect(off).not.toContain("data-usbnet-adapter");
  });
});
