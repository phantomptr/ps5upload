import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));
vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => false }));

import type { FpkgInspection } from "../../api/fpkg";
import { CompressionTiles } from "./CompressionTiles";
import { firmwareLine } from "./GameCard";
import { OptionsCard } from "./OptionsCard";

const estimates = {
  fast: { bytes: 117e9, seconds: 900 },
  balanced: { bytes: 110e9, seconds: 3600 },
  smallest: { bytes: 109e9, seconds: 5400 },
};

describe("CompressionTiles", () => {
  it("is a radio group with the chosen level checked", () => {
    const out = renderToStaticMarkup(
      <CompressionTiles value="balanced" onChange={() => {}} estimates={estimates} />,
    );
    expect(out).toContain('role="radiogroup"');
    expect((out.match(/role="radio"/g) ?? []).length).toBe(3);
    expect(out).toMatch(/aria-checked="true"[^>]*>[\s\S]*?Balanced/);
    expect(out).toContain("Recommended");
  });

  it("shows each level's size and time for this game, or that it is estimating", () => {
    const out = renderToStaticMarkup(
      <CompressionTiles value="fast" onChange={() => {}} estimates={estimates} />,
    );
    expect(out).toContain("~109.0 GB");
    expect(out).toContain("~1 h 0 min");
    const pending = renderToStaticMarkup(
      <CompressionTiles value="fast" onChange={() => {}} estimates="pending" />,
    );
    expect(pending).toContain("Estimating");
  });

  it("shows a dash, not a spinner that never ends, with no game or a failed estimate", () => {
    for (const e of [null, undefined]) {
      const out = renderToStaticMarkup(<CompressionTiles value="fast" onChange={() => {}} estimates={e} />);
      expect(out).not.toContain("Estimating");
      expect(out).toContain("—");
    }
  });
});

describe("OptionsCard free space", () => {
  const props = {
    outputDir: "/out",
    onChangeOutput: () => {},
    onOutputTyped: () => {},
    canBrowse: true,
    compression: "balanced" as const,
    onCompression: () => {},
    estimates: undefined,
    locked: false,
  };

  it("warns before the build when the output drive is short of room", () => {
    const low = renderToStaticMarkup(<OptionsCard {...props} plannedSize={100 * 2 ** 30} outputFree={50 * 2 ** 30} />);
    expect(low).toContain("Low on free space");
    const fine = renderToStaticMarkup(<OptionsCard {...props} plannedSize={100 * 2 ** 30} outputFree={500 * 2 ** 30} />);
    expect(fine).not.toContain("Low on free space");
    expect(fine).toContain("500.00 GiB");
  });
});

describe("GameCard firmware line", () => {
  const base = { source: "s", files: 1, bytes: 1, planned_size: 1, checks: [] } as FpkgInspection;
  const tr = (_k: string, vars?: Record<string, string | number>, fallback?: string) => {
    let s = fallback ?? "";
    for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
    return s;
  };

  it("names the backport's firmware when the modules allow lower", () => {
    expect(firmwareLine({ ...base, required_firmware: "10.20", min_firmware: "4.00" }, tr)).toBe(
      "Runs on FW 4.00 and later (backported)",
    );
  });

  it("falls back to the declared firmware, and says nothing when there is none", () => {
    expect(firmwareLine({ ...base, required_firmware: "05.10", min_firmware: null }, tr)).toBe(
      "Runs on FW 5.10 and later",
    );
    expect(firmwareLine(base, tr)).toBeNull();
  });
});
