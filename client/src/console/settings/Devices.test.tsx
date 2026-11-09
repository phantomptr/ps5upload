import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import en from "../locales/en.json";
import { askLine, DeviceAsks, trustedDevices, viaLine } from "./Devices";

describe("console page › Devices", () => {
  it("names the device asking, and offers Allow and Decline", () => {
    expect(askLine({ id: "a", name: "Pixel 9", ip: "192.168.1.30" })).toBe("Pixel 9 wants to use this PS5.");
    expect(askLine({ id: "a", name: "", ip: "192.168.1.30" })).toBe("192.168.1.30 wants to use this PS5.");
    const html = renderToStaticMarkup(
      <DeviceAsks asks={[{ id: "ab", name: "Pixel 9" }]} busy={false} onAnswer={() => {}} />,
    );
    expect(html).toContain('data-dev-ask="ab"');
    expect(html).toContain('data-action="device-allow"');
    expect(html).toContain('data-action="device-decline"');
    expect(en.cw_dev_allow).toBe("Allow");
    expect(en.cw_dev_decline).toBe("Decline");
    expect(renderToStaticMarkup(<DeviceAsks asks={[]} busy={false} onAnswer={() => {}} />)).toBe("");
  });

  it("says how each device was let in", () => {
    expect(viaLine({ id: "1", name: "a", via: "approved" })).toBe("Asked, and was allowed");
    expect(viaLine({ id: "1", name: "a", via: "tailscale" })).toBe("Let in over Tailscale");
    expect(viaLine({ id: "1", name: "a", via: "network" })).toBe("Let in on the home network");
    expect(viaLine({ id: "1", name: "a", via: "", ip: "192.168.1.5" })).toBe("Paired with a code, or sent the helper");
    // An older helper says nothing: a Tailscale address tells.
    expect(viaLine({ id: "1", name: "a", ip: "100.64.3.4" })).toBe("Let in over Tailscale");
    expect(viaLine({ id: "1", name: "a", ip: "192.168.1.5" })).toBe("Let in on the home network");
  });

  it("lists the devices that may use the PS5, not the blocked ones", () => {
    expect(
      trustedDevices({
        devices: [
          { id: "1", name: "a" },
          { id: "2", name: "b", blocked: true },
        ],
      }).map((d) => d.id),
    ).toEqual(["1"]);
    expect(trustedDevices(null)).toEqual([]);
  });
});
