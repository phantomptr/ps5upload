import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { BackupRunBar, type useBackupRuns } from "./BackupProgress";
import type { BackupRunsView } from "../../api/saveBackup";

type Runs = ReturnType<typeof useBackupRuns>;

function runs(view: BackupRunsView | null, supported = true): Runs {
  return {
    view,
    supported,
    error: null,
    refresh: async () => {},
    cancel: async () => {},
    again: async () => {},
  };
}

const stopped: BackupRunsView = {
  run: {
    id: "share-1",
    which: "share",
    trigger: "manual",
    started_at: 1_700_000_000_000,
    updated_at: 1_700_000_600_000,
    ended_at: 1_700_000_600_000,
    state: "stopped",
    reason_code: "console_gone",
    reason: "The PS5 isn't answering",
    items: [],
    unreached: {},
  },
  summary: {
    id: "share-1",
    trigger: "manual",
    started_at: 1_700_000_000_000,
    ended_at: 1_700_000_600_000,
    state: "stopped",
    reason_code: "console_gone",
    reason: "The PS5 isn't answering",
    total: 66,
    verified: 7,
    failed: 0,
    skipped: 0,
    left: 59,
    read_failed: 0,
    bytes_total: 1,
    bytes_done: 0,
    first_error: null,
    places: {},
  },
  history: [],
  now: 1_700_000_700_000,
};

describe("a backup card's run bar (#895)", () => {
  it("says where a run stopped and offers Continue and the progress view", () => {
    const html = renderToStaticMarkup(
      <BackupRunBar
        which="share"
        runs={runs(stopped)}
        running={false}
        onOpen={() => {}}
        testId="bar"
      />,
    );
    expect(html).toContain(
      "The last backup stopped after 7 of 66 saves: the PS5 stopped answering at",
    );
    expect(html).toContain('data-testid="bar-continue"');
    expect(html).toContain("Backup progress");
  });

  it("offers no Continue while a run runs, nor for one that went through", () => {
    const running = renderToStaticMarkup(
      <BackupRunBar
        which="share"
        runs={runs(stopped)}
        running
        onOpen={() => {}}
        testId="bar"
      />,
    );
    expect(running).not.toContain("bar-continue");
    expect(running).toContain('data-testid="bar-open"');
    const done = renderToStaticMarkup(
      <BackupRunBar
        which="share"
        runs={runs({
          ...stopped,
          run: { ...stopped.run!, state: "done", reason_code: null },
        })}
        running={false}
        onOpen={() => {}}
        testId="bar"
      />,
    );
    expect(done).not.toContain("stopped after");
    expect(done).not.toContain("bar-continue");
  });

  it("shows nothing with an engine that keeps no runs, or before any run", () => {
    expect(
      renderToStaticMarkup(
        <BackupRunBar
          which="local"
          runs={runs(null, false)}
          running={false}
          onOpen={() => {}}
          testId="bar"
        />,
      ),
    ).toBe("");
    const none = renderToStaticMarkup(
      <BackupRunBar
        which="local"
        runs={runs({ run: null, summary: null, history: [], now: 0 })}
        running={false}
        onOpen={() => {}}
        testId="bar"
      />,
    );
    expect(none).not.toContain("bar-open");
  });
});
