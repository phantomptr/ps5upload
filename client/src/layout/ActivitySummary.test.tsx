import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import { summarize, type ActivitySummary } from "../state/activitySummary";
import type { Task } from "../state/tasks";
import { ActivityPanelView, ActivitySummaryLine } from "./ActivitySummary";

// No DOM here: render to markup and check what a user would see and could press.
const NOW = 1_000_000_000;
const noop = () => {};
let n = 0;
function task(p: Partial<Task>): Task {
  n += 1;
  return {
    id: `t${n}`,
    kind: "upload-file",
    origin: "x",
    createdAt: new Date(NOW - 100_000 + n).toISOString(),
    status: "running",
    attempts: 0,
    maxAttempts: 1,
    consoleId: "h",
    payload: {},
    label: `Job ${n}`,
    updatedAtMs: NOW - 1000,
    endedAtMs: null,
    ...p,
  } as Task;
}
const sum = (tasks: Task[], seen: string[] = []) =>
  summarize(tasks, { now: NOW, sessionStart: NOW - 3_600_000, seen: new Set(seen) });
const empty: ActivitySummary = sum([]);
const summaryWith = (count: number) =>
  sum(Array.from({ length: count }, () => task({ progress: { current: 1, total: 2, unit: "bytes" } })));
const summaryWithRows = () =>
  sum([
    task({ id: "a", kind: "fpkg-convert", label: "Convert Spider-Man 2", stage: "Compress" }),
    task({ id: "b", kind: "backup-snapshot", label: "Backup PS5", updatedAtMs: NOW - 125_000 }),
    task({
      id: "c",
      kind: "save-restore",
      label: "Save restore PPSA01",
      status: "failed",
      endedAtMs: NOW - 60_000,
      lastError: { code: "OP_FAILED", message: "USB gone", recoverable: false },
    }),
  ]);

describe("ActivitySummaryLine", () => {
  it("summarises running jobs on one line", () => {
    const out = renderToStaticMarkup(
      <ActivitySummaryLine summary={summaryWith(4)} open={false} onToggle={noop} />,
    );
    expect(out).toContain("4 running");
    expect(out).toContain("Upload 50%");
    expect(out).toContain("+1 more");
  });

  it("says nothing is running, and badges unseen failures", () => {
    const out = renderToStaticMarkup(
      <ActivitySummaryLine summary={{ ...empty, failedUnseen: 2 }} open={false} onToggle={noop} />,
    );
    expect(out).toContain("Nothing running");
    expect(out).toContain("2 failed");
  });

  it("flashes a job that just finished", () => {
    const out = renderToStaticMarkup(
      <ActivitySummaryLine
        summary={{ ...empty, flash: { short: "Backup", label: "Backup PS5", outcome: "done" } }}
        open={false}
        onToggle={noop}
      />,
    );
    expect(out).toContain("Backup PS5 finished");
  });
});

describe("ActivityPanelView", () => {
  it("shows Cancel only where the job can be cancelled, and the stale note", () => {
    const out = renderToStaticMarkup(
      <ActivityPanelView
        summary={summaryWithRows()}
        now={NOW}
        onOpen={noop}
        onCancel={noop}
        onRetry={noop}
        canCancel={(t) => t.id === "a"}
        canRetry={() => false}
      />,
    );
    expect((out.match(/>Cancel</g) ?? []).length).toBe(1);
    expect(out).toContain("Compress");
    expect(out).toContain("No update for 2 min");
    expect(out).toContain("See all activity");
  });

  it("lists what just finished with its reason, and Retry where supported", () => {
    const out = renderToStaticMarkup(
      <ActivityPanelView
        summary={summaryWithRows()}
        now={NOW}
        onOpen={noop}
        onCancel={noop}
        onRetry={noop}
        canCancel={() => false}
        canRetry={(t) => t.id === "c"}
      />,
    );
    expect(out).toContain("Just finished");
    expect(out).toContain("Save restore PPSA01");
    expect(out).toContain("USB gone");
    expect(out).toContain("1 min ago");
    expect((out.match(/>Retry</g) ?? []).length).toBe(1);
  });
});
