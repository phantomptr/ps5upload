import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../state/engine", () => ({ getEngineUrl: () => "http://engine.test" }));

import { WhySlowBody, WhySlowPanel } from "./WhySlow";
import type { JobSummary } from "../../lib/jobSummary";

const summary = (over: Partial<JobSummary> = {}): JobSummary =>
  ({
    schema: 1,
    job_id: "j1",
    kind: "dir",
    console: "0123456789abcdef",
    result: "done",
    code: null,
    message: null,
    files: 3,
    bytes: 100,
    skipped_files: 0,
    skipped_bytes: 0,
    resumed: false,
    attempts: 1,
    drive: "/mnt/usb0",
    engine_version: "5",
    shares: {
      ticks: 10,
      credit_starved_pct: 80,
      source_starved_pct: 5,
      receiver_bound_pct: 71,
    },
    lanes_avg: 4,
    lanes_max: 6,
    chunk_avg_kib: 4096,
    why: { dominant: "receiver_bound", pct: 71, text: "engine text" },
    ...over,
  }) as JobSummary;

describe("why_slow_panel", () => {
  it("renders nothing without a job id", () => {
    expect(renderToStaticMarkup(<WhySlowPanel jobId={undefined} />)).toBe("");
  });

  it("is closed until opened: just the toggle, no request", () => {
    const html = renderToStaticMarkup(<WhySlowPanel jobId="j1" />);
    expect(html).toContain("Why was this slow?");
    expect(html).not.toContain('data-testid="why-slow-body"');
  });

  it("says the dominant share in one sentence, with the numbers behind it", () => {
    const html = renderToStaticMarkup(<WhySlowBody s={summary({ slow_drive_switch: true, resumed: true, settle_ms: 4000, console_line: "apply: 3 files" })} />);
    expect(html).toContain("Console-bound 71 %");
    expect(html).toContain("the console 71 %, the source 5 %");
    expect(html).toContain("4 on average, 6 at most");
    expect(html).toContain("sequential writes");
    expect(html).toContain("reconnected and resumed");
    expect(html).toContain("Finishing on the console took");
    expect(html).toContain("apply: 3 files");
    expect(html).toContain("nothing is sent anywhere");
  });

  it("explains each dominant share with its own sentence", () => {
    const text = (d: JobSummary["why"]["dominant"]) =>
      renderToStaticMarkup(<WhySlowBody s={summary({ why: { dominant: d, pct: 64, text: "" } })} />);
    expect(text("source_starved")).toContain("Source-bound 64 %");
    expect(text("credit_starved")).toContain("Receive window full 64 %");
    expect(text("network")).toContain("network link was the limit");
    expect(text("unmeasured")).toContain("before it could be measured");
  });

  it("skips the numbers for a job too short to measure", () => {
    const html = renderToStaticMarkup(
      <WhySlowBody s={summary({ shares: { ticks: 0 }, lanes_avg: 0, why: { dominant: "unmeasured", pct: 0, text: "" } })} />,
    );
    expect(html).not.toContain("Time held back by");
    expect(html).not.toContain("Connections:");
  });
});
