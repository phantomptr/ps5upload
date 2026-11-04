import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { useNavigate } from "react-router";

import { useActivityPanel } from "../state/activityPanel";
import {
  needsClock,
  summarize,
  type ActivitySummary,
  type QueuedRow,
} from "../state/activitySummary";
import { useConvertQueue } from "../state/convertQueue";
import { queuedWork, stillWaiting } from "../state/queuedWork";
import { profileNameForTask, useRosterStore } from "../state/roster";
import { commandTask, taskCapabilities } from "../state/taskControls";
import { interruptedAtLoad, useTaskStore, type Task } from "../state/tasks";
import { useUploadQueueStore } from "../state/uploadQueue";
import { ActivityPanelView, ActivitySummaryLine, summaryHasNews } from "./ActivitySummary";
import { useBackDismiss } from "../lib/systemBack";

/**
 * One place for everything that takes time: uploads, installs, conversions, dumps, backups,
 * saves, library actions, helper sends… The desktop status strip carries a one-line summary
 * (ActivityStatusSlot); on a phone a pill floats above the tab bar while there is something to
 * say. Either opens the panel: a popover over the strip on a desktop, a bottom sheet on a phone,
 * with every running, waiting and just-finished job.
 *
 * Reads the unified task store, which every long-running feature reports into, and the two
 * queues (console queue, Convert queue) for work that has not started. No polling of its own.
 */
export default function ActivityBar() {
  const open = useActivityPanel((s) => s.open);
  return (
    <>
      <MobilePill />
      {/* The panel's subscriptions exist only while it is open. */}
      {open && <OpenPanel />}
    </>
  );
}

/** The strip's summary slot. */
export function ActivityStatusSlot() {
  const { summary } = useActivitySummary();
  const open = useActivityPanel((s) => s.open);
  const toggle = useActivityPanel((s) => s.toggle);
  return <ActivitySummaryLine summary={summary} open={open} onToggle={toggle} />;
}

/** Phones have no status strip: a pill above the tab bar while anything runs, waits, has just
 *  ended, or failed unseen. Quiet (absent) otherwise. */
function MobilePill() {
  const { summary } = useActivitySummary();
  const open = useActivityPanel((s) => s.open);
  const toggle = useActivityPanel((s) => s.toggle);
  if (open || !summaryHasNews(summary)) return null;
  return (
    <div
      data-testid="activity-pill"
      className="glass-float elev-2 fixed inset-x-3 bottom-[calc(var(--safe-bottom)_+_5.1rem)] z-30 rounded-full ps-[var(--safe-left)] pe-[var(--safe-right)] md:hidden"
    >
      <ActivitySummaryLine summary={summary} open={false} onToggle={toggle} variant="pill" />
    </div>
  );
}

function OpenPanel() {
  // Live while open: the clock ticks so "Just finished" ages, and restarts at the real time.
  const { summary, now } = useActivitySummary(true);
  const navigate = useNavigate();
  const close = useActivityPanel((s) => s.close);
  const markSeen = useActivityPanel((s) => s.markSeen);
  const profiles = useRosterStore((s) => s.profiles);

  // A failure that lands while the panel is open has been seen.
  const failedIds = summary.finished.filter((r) => r.outcome === "failed").map((r) => r.id).join(",");
  useEffect(() => {
    if (failedIds) markSeen(failedIds.split(","));
  }, [failedIds, markSeen]);

  // Opening moves focus into the panel; Escape closes it and returns focus to the button that
  // opened it (the strip's, or the phone pill's — gone while open, so the strip's or none).
  const wrapRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    wrapRef.current?.focus({ preventScroll: true });
  }, []);
  const closeToToggle = () => {
    close();
    const toggles = document.querySelectorAll<HTMLElement>("[data-activity-toggle]");
    for (const t of toggles) {
      if (t.offsetParent !== null) {
        t.focus();
        break;
      }
    }
  };

  useBackDismiss(true, closeToToggle);

  return (
    <>
      {/* Click outside closes; on a phone it also dims what is behind the sheet. */}
      <div
        aria-hidden
        data-testid="activity-backdrop"
        onClick={close}
        className="anim-scrim fixed inset-0 z-40 bg-black/40 md:bg-transparent"
      />
      <div
        ref={wrapRef}
        tabIndex={-1}
        data-testid="activity-sheet"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            closeToToggle();
          }
        }}
        className={[
          "glass-float elev-2 anim-sheet fixed z-50 overflow-y-auto overscroll-contain outline-none",
          // Phone: a bottom sheet inside the safe area.
          "inset-x-0 bottom-0 max-h-[80dvh] rounded-t-[var(--radius-card)] pt-4",
          "pb-[calc(var(--safe-bottom)_+_1rem)] ps-[calc(var(--safe-left)_+_1rem)] pe-[calc(var(--safe-right)_+_1rem)]",
          // Desktop: a popover over the status strip's right end.
          "md:inset-x-auto md:bottom-[calc(var(--safe-bottom)_+_2.75rem)] md:end-[calc(var(--safe-right)_+_1.5rem)] md:max-h-[min(36rem,70vh)] md:w-[min(34rem,calc(100vw-3rem))] md:rounded-[var(--radius-card)] md:px-5 md:py-4",
        ].join(" ")}
      >
        <ActivityPanelView
          summary={summary}
          now={now}
          onOpen={(route) => {
            close();
            navigate(route);
          }}
          onClose={closeToToggle}
          onCancel={(task) => void commandTask(task, "cancel")}
          onRetry={(task) => void commandTask(task, "retry")}
          onRemoveQueued={removeQueued}
          canCancel={(task) => taskCapabilities(task).canCancel}
          canRetry={(task) => taskCapabilities(task).canRetry}
          consoleOf={(task) =>
            profiles.length > 1 && task.consoleId ? profileNameForTask(task, profiles) : null
          }
        />
      </div>
    </>
  );
}

function removeQueued(row: QueuedRow): void {
  // The row is from an earlier render: if its item started since, removing it would stop a
  // running job without the confirmation Cancel asks for. Then do nothing; the row is gone
  // from the next render and the running job has its own Cancel.
  if (
    !stillWaiting(row, useUploadQueueStore.getState().items, useConvertQueue.getState().items)
  ) {
    return;
  }
  if (row.owner === "upload-queue") useUploadQueueStore.getState().cancelItem(row.id);
  else useConvertQueue.getState().remove(row.id);
}

// ── One shared once-a-second clock ─────────────────────────────────────────
// The strip, the phone pill and the open panel all read the clock; one interval serves them,
// and only while one of them needs it (the panel open, a job running, which can go stale, or one
// that just ended, which flashes). The interval stops otherwise, so its reading goes stale: it
// is re-read from Date.now() whenever the interval starts again (the panel opening, a job
// starting), never carried over.

const clockListeners = new Set<() => void>();
let clockNow = Date.now();
let clockTimer: number | null = null;

function subscribeClock(cb: () => void): () => void {
  clockListeners.add(cb);
  if (clockTimer === null) {
    // useSyncExternalStore re-reads the snapshot after subscribing, so this fresh reading is
    // rendered at once.
    clockNow = Date.now();
    clockTimer = window.setInterval(() => {
      clockNow = Date.now();
      for (const l of clockListeners) l();
    }, 1000);
  }
  return () => {
    clockListeners.delete(cb);
    if (clockListeners.size === 0 && clockTimer !== null) {
      window.clearInterval(clockTimer);
      clockTimer = null;
    }
  };
}
const noSubscribe = () => () => {};
const readClock = () => clockNow;

function useClock(active: boolean): number {
  return useSyncExternalStore(active ? subscribeClock : noSubscribe, readClock, readClock);
}

/** The summary of the task store and the queues, refreshed by the shared clock while anything in
 *  it depends on time (always while `live`: the open panel). */
function useActivitySummary(live = false): { summary: ActivitySummary; now: number } {
  const tasks = useTaskStore((s) => s.tasks);
  const seen = useActivityPanel((s) => s.seen);
  const sessionStart = useActivityPanel((s) => s.sessionStart);
  const queueItems = useUploadQueueStore((s) => s.items);
  const convertItems = useConvertQueue((s) => s.items);
  const queued = useMemo(() => queuedWork(queueItems, convertItems), [queueItems, convertItems]);
  const latest = useMemo(() => latestStamp(tasks), [tasks]);
  const clockBound = needsClock(tasks, readClock(), latest, live);
  const ticked = useClock(clockBound);
  // Every task change stamps the time it happened, so the newest stamp is "now" whenever the
  // store moves; the clock carries the time between changes.
  const now = Math.max(ticked, latest);
  const summary = useMemo(
    () => summarize(tasks, { now, sessionStart, seen, interruptedAtLoad, queued }),
    [tasks, now, sessionStart, seen, queued],
  );
  return { summary, now };
}

function latestStamp(tasks: readonly Task[]): number {
  let latest = 0;
  for (const t of tasks) latest = Math.max(latest, t.updatedAtMs, t.endedAtMs ?? 0);
  return latest;
}
