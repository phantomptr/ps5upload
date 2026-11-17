import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { PmOverview } from "../../../api/payloadManager";
import { PmSummary } from "./summaries";

const view = (over: Partial<Extract<PmOverview, { present: true }>> = {}): PmOverview => ({
  present: true,
  version: "0.6.1",
  payloads: [
    { path: "/data/pldmgr/payloads/local/a/a.elf", slot: "local/a", filename: "a.elf", folder: "local/a", name: "", version: "", source: "", install_source: "" },
    { path: "/data/pldmgr/payloads/local/b/b.elf", slot: "local/b", filename: "b.elf", folder: "local/b", name: "", version: "", source: "", install_source: "" },
  ],
  payloadsError: null,
  autoload: null,
  autoloadError: null,
  ...over,
});

const html = (v: PmOverview | null | undefined, running: number | null) =>
  renderToStaticMarkup(<PmSummary view={v} running={running} onReport={() => undefined} fallback="FALLBACK" />);

describe("Payload Manager's card line", () => {
  it("counts saved and running", () => {
    expect(html(view(), 1)).toBe("2 saved · 1 running");
  });

  it("says only what is stored while what runs isn't matched yet", () => {
    expect(html(view(), null)).toBe("Payloads stored: 2");
  });

  it("doesn't call a list it couldn't read empty", () => {
    expect(html(view({ payloads: [], payloadsError: "HTTP 500" }), 0)).toBe("FALLBACK");
  });

  it("falls back without Payload Manager", () => {
    expect(html({ present: false, error: "not running" }, null)).toBe("FALLBACK");
    expect(html(undefined, null)).toBe("FALLBACK");
  });
});
