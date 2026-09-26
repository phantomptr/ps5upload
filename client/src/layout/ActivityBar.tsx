import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";

import { useActivityPanel } from "../state/activityPanel";
import { summarize, type ActivitySummary } from "../state/activitySummary";
import { profileNameForAddr, useRosterStore } from "../state/roster";
import { commandTask, taskCapabilities } from "../state/taskControls";
import { isTerminal, useTaskStore, type Task } from "../state/tasks";
import { ActivityPanelView, ActivitySummaryLine } from "./ActivitySummary";

/**
 * One place for everything that takes time: uploads, installs, conversions, backups, saves,
 * library actions… The status strip carries a one-line summary (ActivityStatusSlot); clicking it
 * opens this panel above the strip with a row per job and what just finished.
 *
 * Reads only the unified task store, which every long-running feature reports into.
 */
export default function ActivityBar() {
  const open = useActivityPanel((s) => s.open);
  // The panel's subscriptions (and its once-a-second tick) exist only while it is open.
  return open ? <OpenPanel /> : null;
}

/** The strip's summary slot. */
export function ActivityStatusSlot() {
  const { summary } = useActivitySummary();
  const open = useActivityPanel((s) => s.open);
  const toggle = useActivityPanel((s) => s.toggle);
  return <ActivitySummaryLine summary={summary} open={open} onToggle={toggle} />;
}

function OpenPanel() {
  const { summary, now } = useActivitySummary();
  const navigate = useNavigate();
  const close = useActivityPanel((s) => s.close);
  const markSeen = useActivityPanel((s) => s.markSeen);
  const profiles = useRosterStore((s) => s.profiles);

  // A failure that lands while the panel is open has been seen.
  const failedIds = summary.finished.filter((r) => r.outcome === "failed").map((r) => r.id).join(",");
  useEffect(() => {
    if (failedIds) markSeen(failedIds.split(","));
  }, [failedIds, markSeen]);

  return (
    <div className="hidden md:block">
      <ActivityPanelView
        summary={summary}
        now={now}
        onOpen={(route) => {
          close();
          navigate(route);
        }}
        onCancel={(task) => void commandTask(task, "cancel")}
        onRetry={(task) => void commandTask(task, "retry")}
        canCancel={(task) => taskCapabilities(task).canCancel}
        canRetry={(task) => taskCapabilities(task).canRetry}
        consoleOf={(task) =>
          profiles.length > 1 && task.consoleId ? profileNameForAddr(task.consoleId, profiles) : null
        }
      />
    </div>
  );
}

/** The summary of the task store, refreshed once a second while anything in it depends on the
 *  clock (a job running, which can go stale, or one that just ended, which flashes). */
function useActivitySummary(): { summary: ActivitySummary; now: number } {
  const tasks = useTaskStore((s) => s.tasks);
  const seen = useActivityPanel((s) => s.seen);
  const sessionStart = useActivityPanel((s) => s.sessionStart);
  const [ticked, setTicked] = useState(() => Date.now());
  // Every task change stamps the time it happened, so the newest stamp is "now" whenever the
  // store moves; the tick carries the clock between changes.
  const now = useMemo(() => Math.max(ticked, latestStamp(tasks)), [ticked, tasks]);
  const clockBound = tasks.some(
    (t) => !isTerminal(t.status) || (t.endedAtMs != null && now - t.endedAtMs < 6000),
  );
  useEffect(() => {
    if (!clockBound) return;
    const id = window.setInterval(() => setTicked(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [clockBound]);
  const summary = useMemo(
    () => summarize(tasks, { now, sessionStart, seen }),
    [tasks, now, sessionStart, seen],
  );
  return { summary, now };
}

function latestStamp(tasks: readonly Task[]): number {
  let latest = 0;
  for (const t of tasks) latest = Math.max(latest, t.updatedAtMs, t.endedAtMs ?? 0);
  return latest;
}
