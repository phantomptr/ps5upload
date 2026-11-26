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

import { ConsoleStorageCard } from "./index";

describe("the Console Storage card (#944)", () => {
  const out = renderToStaticMarkup(
    <ConsoleStorageCard
      storage={{
        total_bytes: 2005599387648,
        free_bytes: 182746480640,
        used_bytes: 1822852907008,
        reserved_bytes: 307929022464,
        user_total_bytes: 1995416338432,
        user_free_bytes: 175467462656,
        user_reserved_bytes: 307929022464,
        system_data_total_bytes: 8572829696,
        system_data_free_bytes: 7136804864,
        system_ex_total_bytes: 1610219520,
        system_ex_free_bytes: 142213120,
      }}
    />,
  );

  it("shows GiB like the rest of the app, and the free space Volumes shows", () => {
    expect(out).toContain("1.81 TiB");
    expect(out).toContain("163 GiB");
    expect(out).not.toMatch(/\d (GB|TB)\b/);
    // Not the three partitions' 170 GiB.
    expect(out).not.toContain("170 GiB");
  });

  it("names what is held back and says it is not free space", () => {
    expect(out).toContain("Held back by the PS5");
    expect(out).toContain("287 GiB");
    expect(out).toContain("Counted in Used. Not free space.");
  });

  it("does not claim to match the PS5's Storage screen, and says it can show less", () => {
    expect(out).toContain("As the drive&#x27;s filesystem counts it.");
    expect(out).toContain("can show less free space");
    // The note sits outside the list of figures.
    expect(out).toMatch(/<\/dl><p[^>]*data-testid="hw-storage-note"/);
  });
});
