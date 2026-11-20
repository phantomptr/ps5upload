import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { LinkDropBannerView } from "./LinkDropBanner";
import type { LinkHint, LinkHistory } from "../lib/linkHistory";

const hint = (kind: LinkHint["kind"]): LinkHint => ({ kind, drops: 3, since_ms: 1, last_ms: 2 });
const view = (h: LinkHint | null, link: LinkHistory["link"] = null) =>
  renderToStaticMarkup(<LinkDropBannerView hint={h} link={link} onDismiss={() => {}} />);

describe("connection keeps dropping banner", () => {
  it("names the USB drive and the ways out when the drops began with one on Wi-Fi", () => {
    const html = view(hint("usb_wifi"), { kind: "wifi", iface: "wlan0", at_ms: 1 });
    expect(html).toContain("keeps dropping since a USB drive was connected");
    expect(html).toContain("2.4 GHz Wi-Fi");
    expect(html).toContain("extension cable");
    expect(html).toContain("Dismiss");
  });

  it("says only that it keeps dropping otherwise, without the USB sentence", () => {
    const html = view(hint("keeps_dropping"));
    expect(html).toContain("The connection to the PS5 keeps dropping.");
    expect(html).toContain("5 GHz Wi-Fi");
    expect(html).not.toContain("USB");
  });

  it("on a cable, it does not suggest a cable", () => {
    const html = view(hint("keeps_dropping"), { kind: "wired", iface: "eth0", at_ms: 1 });
    expect(html).toContain("Check the network cable");
    expect(html).not.toContain("Wi-Fi");
  });

  it("is silent with no hint", () => {
    expect(view(null)).toBe("");
  });

  it("never names another tool or payload", () => {
    const html = view(hint("usb_wifi"));
    expect(html).not.toMatch(/mount|payload/i);
  });
});
