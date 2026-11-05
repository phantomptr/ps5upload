import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { parseHbStatus, type HbOverview } from "../../../api/hearbridge";
import { HbSummary, HearBridgePanel, hbPageUrl, hbSummaryParts } from "./hearbridge";

// No DOM here (vitest runs in node): static markup, asserting on what is shown.
const tr = ((_k: string, vars: Record<string, unknown> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_, n) => String(vars?.[n] ?? ""))) as never;

const streaming = parseHbStatus({
  version: "1.9.0",
  state: "streaming",
  device: "WH-1000XM4",
  gain_pct: 140,
  muted: 0,
  codec: "SBC",
  latency: { target_ms: 120, estimate_ms: 185 },
  lat_extra: { auto: 0 },
  battery: { pct: 70 },
  link: { score: 88 },
  night: { on: 0 },
  game: { name: "Astro Bot", profile: 1 },
  events: ["Connected", "Streaming"],
});

const present: HbOverview = { present: true, port: 8091, status: streaming };
const noop = () => undefined;
const noopAsync = async () => undefined;

describe("HearBridge card", () => {
  it("sums up the headset in one line", () => {
    expect(hbSummaryParts(streaming, tr)).toEqual([
      "Streaming",
      "WH-1000XM4",
      "Battery 70%",
      "Link 88%",
      "185 ms delay",
      "SBC",
      "Volume 140%",
      "Profile: Astro Bot",
    ]);
  });

  it("says less while waiting, and nothing it doesn't know", () => {
    const waiting = parseHbStatus({ version: "0.3", state: "disconnected", muted: 1 });
    expect(hbSummaryParts(waiting, tr)).toEqual(["Waiting for a headset", "Muted"]);
  });

  it("has its own line while the first status read is out", () => {
    expect(renderToStaticMarkup(<HbSummary view={null} />)).toContain("Reading HearBridge");
  });

  it("says HearBridge didn't answer instead of failing", () => {
    expect(renderToStaticMarkup(<HbSummary view={{ present: false, error: "x" }} />)).toContain(
      "HearBridge didn&#x27;t answer",
    );
  });

  it("opens HearBridge's page on the port it answered on", () => {
    expect(hbPageUrl("192.0.2.99", 8091)).toBe("http://192.0.2.99:8091/");
    expect(hbPageUrl("192.0.2.99", null)).toBe("http://192.0.2.99:8090/");
  });
});

describe("HearBridge Manage panel", () => {
  it("has the everyday controls and points to HearBridge's page for the rest", () => {
    const html = renderToStaticMarkup(
      <HearBridgePanel host="192.0.2.99" view={present} apply={noop} refresh={noopAsync} />,
    );
    expect(html).toContain('max="500"'); // volume, HearBridge's 0..500 %
    expect(html).toContain('max="400"'); // fixed latency, 40..400 ms
    expect(html).toContain("Night mode");
    expect(html).toContain("Reconnect");
    expect(html).toContain("Scan for headsets");
    expect(html).toContain("Recent events");
    expect(html).toContain("Open HearBridge page");
  });

  it("falls back to the page link when HearBridge doesn't answer", () => {
    const html = renderToStaticMarkup(
      <HearBridgePanel
        host="192.0.2.99"
        view={{ present: false, error: "HearBridge didn't answer on 192.0.2.99 port 8090-8095" }}
        apply={noop}
        refresh={noopAsync}
      />,
    );
    expect(html).toContain("Open HearBridge page");
    expect(html).not.toContain('type="range"');
  });
});
