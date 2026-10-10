import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr:
    () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import { KeptByAppView } from "./KeptByApp";
import { formatBytes } from "../../lib/format";
import type { UsageRow } from "../../lib/ps5uploadUsage";

const row = (over: Partial<UsageRow>): UsageRow => ({
  key: "pkg_temp",
  drive: "/data",
  path: "/user/data/ps5upload/pkg_temp",
  cleanable: true,
  exists: true,
  bytes: 5 * 1024 ** 3,
  files: 3,
  truncated: false,
  ...over,
});

const view = (rows: UsageRow[] | null, extra = {}) =>
  renderToStaticMarkup(
    <KeptByAppView rows={rows} busyPath={null} onClean={() => {}} onOpen={() => {}} {...extra} />,
  );

describe("what ps5upload keeps, on Volumes", () => {
  it("lists each folder with its drive and size, and offers clean-up only where it is safe", () => {
    const html = view([
      row({}),
      row({ key: "pkg_library", path: "/mnt/ext0/ps5upload/pkg_library", drive: "/mnt/ext0", cleanable: false, bytes: 40 * 1024 ** 3, files: 2 }),
    ]);
    expect(html).toContain("Package temp files");
    expect(html).toContain(formatBytes(5 * 1024 ** 3));
    expect(html).toContain("Package library");
    expect(html).toContain("/mnt/ext0");
    expect(html).toContain('data-testid="kept-clean-/user/data/ps5upload/pkg_temp"');
    // The library is the user's packages: it can be opened, never emptied from here.
    expect(html).not.toContain('data-testid="kept-clean-/mnt/ext0/ps5upload/pkg_library"');
    expect(html).toContain('data-testid="kept-open-/mnt/ext0/ps5upload/pkg_library"');
  });

  it("says so when ps5upload keeps nothing", () => {
    expect(view([])).toContain("ps5upload is not keeping anything on this console");
  });

  it("marks a size that was cut short as a floor", () => {
    expect(view([row({ truncated: true })])).toContain("at least");
  });

  it("shows nothing while it has not measured yet", () => {
    expect(view(null)).toContain("Measuring");
  });
});
