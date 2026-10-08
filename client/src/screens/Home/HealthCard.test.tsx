import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr:
    () =>
    (
      key: string,
      vars?: Record<string, string | number>,
      fallback?: string,
    ) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {}))
        s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import { HealthCardView } from "./HealthCard";
import type { HealthCheck } from "../../api/ps5";

const check = (over: Partial<HealthCheck>): HealthCheck => ({
  id: "x",
  title: "PS5 can reach this computer",
  category: "network",
  status: "fail",
  detail: "The PS5 could not connect to http://192.168.1.20:19113.",
  remedy: "Allow ps5upload through the firewall.",
  ...over,
});

const html = (p: Parameters<typeof HealthCardView>[0]) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <HealthCardView {...p} />
    </MemoryRouter>,
  );

describe("Home health card", () => {
  it("says so plainly when nothing needs attention, with a way to the full check", () => {
    const out = html({
      problems: [],
      scanned: true,
      scanning: false,
      onScan: () => {},
    });
    expect(out).toContain("Everything checks out");
    expect(out).toContain('href="/health"');
  });

  it("shows each thing that needs attention with what to do about it", () => {
    const out = html({
      problems: [
        check({}),
        check({
          id: "y",
          status: "warn",
          title: "Reply time",
          detail: "90 ms",
          remedy: "Use a cable.",
        }),
      ],
      scanned: true,
      scanning: false,
      onScan: () => {},
    });
    expect(out).toContain("2 things need attention");
    expect(out).toContain("PS5 can reach this computer");
    expect(out).toContain("Allow ps5upload through the firewall.");
    expect(out).toContain("Reply time");
    expect(out).not.toContain("Everything checks out");
  });

  it("keeps the list short and says how many more there are", () => {
    const many = Array.from({ length: 6 }, (_, i) =>
      check({ id: `c${i}`, title: `Check ${i}` }),
    );
    const out = html({
      problems: many,
      scanned: true,
      scanning: false,
      onScan: () => {},
    });
    expect(out).toContain("Check 2");
    expect(out).not.toContain("Check 3");
    expect(out).toContain("3 more");
  });

  it("before the first scan it says it is checking, not that all is well", () => {
    const out = html({
      problems: [],
      scanned: false,
      scanning: true,
      onScan: () => {},
    });
    expect(out).toContain("Checking");
    expect(out).not.toContain("Everything checks out");
  });
});
