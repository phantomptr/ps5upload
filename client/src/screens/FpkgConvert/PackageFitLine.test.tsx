import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.split(`{${k}}`).join(String(v));
      return s;
    },
}));

import type { FpkgEstimates, FpkgInspection } from "../../api/fpkg";
import { GIB, LARGEST_MEASURED_INDEX } from "../../lib/packageLimit";
import { fitFor, PackageFitLine } from "./PackageFitLine";

const game = (over: Partial<FpkgInspection> = {}): FpkgInspection => ({
  source: "folder /g",
  files: 194,
  bytes: 540 * GIB,
  required_firmware: "12.00",
  planned_size: 541 * GIB,
  checks: [],
  param_sizes: { ac_gib: 64, app_gib: 320 },
  layout_index_fixed: 20_000_000,
  ...over,
});

const est = (balanced: number): FpkgEstimates => ({
  fast: { bytes: balanced * 1.05, seconds: 1 },
  balanced: { bytes: balanced, seconds: 2 },
  smallest: { bytes: balanced * 0.99, seconds: 3 },
});

const line = (inspection: FpkgInspection, ...rest: Parameters<typeof fitFor> extends [unknown, ...infer R] ? R : never) => {
  const fit = fitFor(inspection, ...rest);
  return fit ? renderToStaticMarkup(<PackageFitLine fit={fit} onMakeImage={() => {}} />) : null;
};

describe("PackageFitLine", () => {
  it("shows the size, the ceiling with the add-on space, and what is left", () => {
    // 216.4 GiB compressed, plus the container's digest tables (~0.1%) and the 64 GiB reserved.
    const html = line(game(), "balanced", est(216.4 * GIB), "13.60");
    expect(html).toContain("Fits the PS5&#x27;s 450 GiB limit on FW 13.x: ~280.6 of 450 GiB incl. 64 GiB add-on space, 169.4 GiB to spare");
  });

  it("is an error past the ceiling, with a game image as the way out", () => {
    const html = line(game(), "stored", null, "13.60")!;
    expect(html).toContain('role="alert"');
    expect(html).toContain(
      "Too big for the PS5: this package would be 541.6 GiB, and FW 13.x can&#x27;t open a fake package larger than 450 GiB including the add-on space this game reserves (64 GiB). It would install but never start.",
    );
    expect(html).toContain("Make a game image instead");
    // The same game compressed fits.
    expect(line(game(), "balanced", est(200 * GIB), "13.60")).toContain("Fits");
  });

  it("uses the game's own 320 GiB profile on 11.xx and 12.xx", () => {
    expect(line(game(), "balanced", est(300 * GIB), "12.00")).toContain("Fits the PS5&#x27;s 320 GiB limit on FW 11.xx–12.xx");
    expect(line(game({ param_sizes: {} }), "balanced", est(300 * GIB), "12.00")).toContain("can&#x27;t open a fake package larger than 256 GiB");
  });

  it("with no console known, checks 13.x and says what older firmware won't open", () => {
    const html = line(game(), "balanced", est(300 * GIB), null)!;
    expect(html).toContain("Fits the PS5&#x27;s 450 GiB limit on FW 13.x");
    expect(html).toContain("it opens on FW 13.x only");
  });

  it("waits for the compressed size when the uncompressed one would not fit", () => {
    expect(line(game(), "balanced", "pending", "13.60")).toBeNull();
    const small = game({ planned_size: 20 * GIB, bytes: 20 * GIB });
    expect(line(small, "balanced", "pending", "13.60")).toContain("Fits");
  });

  it("says when there's no size check, and never shows for a PS4 game", () => {
    expect(line(game(), "stored", null, "5.10")).toContain("No package size limit is known on FW 5.10.");
    expect(line(game({ ps4_game: true }), "stored", null, "13.60")).toBeNull();
  });

  it("warns about a layout index past the largest measured", () => {
    const big = game({ layout_index_fixed: LARGEST_MEASURED_INDEX });
    expect(line(big, "balanced", est(100 * GIB), "13.60")).toContain("layout index would be about");
    expect(line(game({ layout_index_fixed: 1000 }), "balanced", est(100 * GIB), "13.60")).not.toContain("layout index");
  });
});
