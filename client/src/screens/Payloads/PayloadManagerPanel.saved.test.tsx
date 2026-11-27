import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { PmOverview, PmStoredPayload } from "../../api/payloadManager";
import PayloadManagerPanel from "./PayloadManagerPanel";
import { payloadCount } from "./hub/pmMerge";
import { usePmViewStore } from "./hub/pmViewStore";

vi.mock("../../state/connection", () => ({
  useConnectionStore: (sel: (s: { host: string }) => unknown) =>
    sel({ host: "192.168.1.50" }),
}));

// No DOM here (vitest runs in node): static markup of the "On your PS5" saved list.
const HOST = "192.168.1.50";
const saved = (i: number): PmStoredPayload => ({
  path: `/data/pldmgr/payloads/local/p${i}/payload-${i}.elf`,
  slot: `local/p${i}`,
  filename: `payload-${i}.elf`,
  folder: `p${i}`,
  name: `Payload ${i}`,
  version: "",
  source: "",
  install_source: "",
});
const overview = (n: number): PmOverview => ({
  present: true,
  version: "0.6.0",
  payloads: Array.from({ length: n }, (_, i) => saved(i + 1)),
  payloadsError: null,
  autoload: { enabled: true, delay_s: 0, list: ["local/p1"] },
  autoloadError: null,
});
const count = (html: string, needle: string) => html.split(needle).length - 1;

describe("Your payloads on the On your PS5 tab", () => {
  it("lists every saved payload from the tab's overview, each with its actions", () => {
    usePmViewStore.getState().put(HOST, overview(20));
    const html = renderToStaticMarkup(<PayloadManagerPanel part="saved" />);
    expect(html).toContain('data-testid="pm-saved-list"');
    expect(html).toContain("Your payloads · 20");
    expect(count(html, 'data-testid="pm-row-name"')).toBe(20);
    expect(count(html, 'data-testid="pm-row-actions"')).toBe(20);
    expect(count(html, "Launch</")).toBe(20);
    // A boot toggle on every row: one is loaded at boot, the rest can be added.
    expect(count(html, "load at boot</")).toBe(1);
    expect(count(html, "Load at boot</")).toBe(19);
    // Payload Manager's own settings stay under Manage on its card.
    expect(html).not.toContain("Payload Manager 0.6.0 on the PS5");
  });

  it("shows a running saved payload's Stop on its row", () => {
    usePmViewStore.getState().put(HOST, overview(3));
    const running = {
      "/data/pldmgr/payloads/local/p2/payload-2.elf": {
        pid: 120,
        memoryMib: 4,
        ours: false,
        canStop: true,
        busy: false,
        onStop: () => undefined,
        variants: [],
        webUrl: null,
      },
    };
    const html = renderToStaticMarkup(
      <PayloadManagerPanel part="saved" running={running} />,
    );
    expect(html).toContain("Running · PID 120");
    expect(count(html, "Stop</")).toBe(1);
  });

  it("lists as many rows as the tab's count adds for them", () => {
    const view = overview(20);
    usePmViewStore.getState().put(HOST, view);
    const html = renderToStaticMarkup(<PayloadManagerPanel part="saved" />);
    const rows = count(html, 'data-testid="pm-row-name"');
    // The report's console: 4 detected cards and 1 also running, plus the saved rows.
    const savedRows = view.present ? view.payloads.length : 0;
    expect(payloadCount(5, savedRows)).toBe(5 + rows);
    expect(rows).toBe(20);
  });
});
