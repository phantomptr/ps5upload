import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../state/lang", () => ({
  useLangStore: { getState: () => ({ lang: "en" }) },
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import { summarize, type ActivitySummary, type QueuedRow } from "../state/activitySummary";
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
const queuedRow = (id: string): QueuedRow => ({
  id,
  owner: "upload-queue",
  kind: "pkg-install",
  label: `Queued ${id}`,
  consoleId: "h",
  route: "/send",
  sizeBytes: 4 * 1024 ** 3,
});
const summaryWithRows = () =>
  sum([
    task({ id: "a", kind: "fpkg-convert", label: "Convert Spider-Man 2", stage: "Compress" }),
    task({
      id: "b",
      kind: "backup-snapshot",
      label: "Backup PS5",
      updatedAtMs: NOW - 125_000,
      progress: { current: 1, total: 4, unit: "items" },
    }),
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
  it("names the job it shows, how far, how fast and how long, and counts the rest", () => {
    const s = sum([
      task({
        label: "Spider-Man 2.pkg",
        stage: "Sending",
        progress: { current: 1, total: 2, unit: "bytes" },
        rate: { bytesPerSec: 50 * 1024 * 1024 },
        eta: 125,
      }),
      task({}),
      task({}),
    ]);
    const out = renderToStaticMarkup(
      <ActivitySummaryLine summary={{ ...s, queued: [queuedRow("q")], others: 3 }} open={false} onToggle={noop} />,
    );
    expect(out).toContain("Upload");
    expect(out).toContain("Spider-Man 2.pkg");
    expect(out).toContain("50% · 50.0 MiB/s · 2m 5s left");
    // The step and byte counts are in the spoken label.
    expect(out).toContain("Sending, 50%, 1 B of 2 B");
    expect(out).toContain("+3");
    // The slim bar: half full.
    expect(out).toContain("width:50%");
  });

  it("says nothing is running, and badges unseen failures", () => {
    const out = renderToStaticMarkup(
      <ActivitySummaryLine summary={{ ...empty, failedUnseen: 2 }} open={false} onToggle={noop} />,
    );
    expect(out).toContain("Nothing running");
    expect(out).toContain("2 failed");
  });

  it("says the step once every byte is sent, and the phone pill keeps only how far and how long", () => {
    const finishing = sum([
      task({ stage: "Finishing on the PS5", progress: { current: 2, total: 2, unit: "bytes" }, rate: { bytesPerSec: 9 } }),
    ]);
    const strip = renderToStaticMarkup(<ActivitySummaryLine summary={finishing} open={false} onToggle={noop} />);
    expect(strip).toContain(">100% · Finishing on the PS5</span>");
    const busy = sum([
      task({ progress: { current: 1, total: 4, unit: "bytes" }, rate: { bytesPerSec: 1024 }, eta: 30 }),
    ]);
    const pill = renderToStaticMarkup(
      <ActivitySummaryLine summary={busy} open={false} onToggle={noop} variant="pill" />,
    );
    expect(pill).toContain("25% · 30s left");
  });

  it("counts queued work when nothing has started", () => {
    const out = renderToStaticMarkup(
      <ActivitySummaryLine summary={{ ...empty, queued: [queuedRow("a"), queuedRow("b")] }} open={false} onToggle={noop} />,
    );
    expect(out).toContain("2 waiting");
  });

  it("says a job that went quiet has gone quiet", () => {
    const out = renderToStaticMarkup(
      <ActivitySummaryLine summary={summaryWithRows()} open={false} onToggle={noop} />,
    );
    // The Convert row still reports (fresh), so it is the one named.
    expect(out).toContain("Convert Spider-Man 2");
    expect(out).toContain("+1");
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

describe("ActivityPanelView details", () => {
  it("lists running, waiting and finished work with figures and controls", () => {
    const s = sum([
      task({ label: "Big.exfat", progress: { current: 1024 ** 3, total: 4 * 1024 ** 3, unit: "bytes" } }),
      task({ status: "done", label: "Old", endedAtMs: NOW - 10_000 }),
    ]);
    const removed: string[] = [];
    const out = renderToStaticMarkup(
      <ActivityPanelView
        summary={{ ...s, queued: [queuedRow("q1")] }}
        now={NOW}
        onOpen={noop}
        onCancel={noop}
        onRetry={noop}
        onClose={noop}
        onRemoveQueued={(r) => removed.push(r.id)}
        canCancel={() => true}
        canRetry={() => false}
        consoleOf={() => "Pro"}
      />,
    );
    expect(out).toContain("Running");
    expect(out).toContain("Waiting");
    expect(out).toContain("1 running · 1 waiting");
    expect(out).toContain("25% · 1.00 GiB of 4.00 GiB");
    expect(out).toContain("Queued q1");
    expect(out).toContain("4.00 GiB");
    expect(out).toContain(">Remove<");
    expect(out).toContain("→ Pro");
    expect(out).toContain('aria-label="Close"');
  });
});

describe("failureText", () => {
  it("says an engine token in words and leaves other text alone", async () => {
    const { failureText } = await import("./ActivitySummary");
    expect(failureText("ava1_unreachable")).toMatch(/could not be reached/);
    expect(failureText("console_rest_mode")).toMatch(/rest mode/);
    expect(failureText("USB gone")).toBe("USB gone");
    expect(failureText("some_unknown_token")).toBe("some_unknown_token");
  });
});

describe("shownEta", () => {
  it("shows a real time left, never one from a vanishing speed", async () => {
    const { shownEta } = await import("./ActivitySummary");
    expect(shownEta(90)).toBe(true);
    expect(shownEta(1.42e218 * 3600)).toBe(false);
    expect(shownEta(Infinity)).toBe(false);
    expect(shownEta(0)).toBe(false);
    expect(shownEta(undefined)).toBe(false);
  });
});
