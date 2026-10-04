import { useEffect, useMemo, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  CheckCircle2,
  XCircle,
  CircleDashed,
  ChevronRight,
  Play,
  Square,
  Trash2,
  ScanSearch,
  RotateCcw,
  ListOrdered,
  X,
  Ban,
  Info,
  UploadCloud,
} from "lucide-react";

import {
  Button,
  ErrorCard,
  ProgressBar,
  Spinner,
  Toggle,
} from "../../components";
import { queueItemViewPath } from "../../lib/queueView";
import { usePackageViewer } from "../../state/packageViewer";
import { GameIcon } from "../../components/GameIcon";
import { PlatformBadge } from "../../components/PlatformBadge";
import { humanizeJobErrorReason } from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import {
  platformForTitleId,
  titleIdFromContentId,
} from "../../lib/titleDetails";
import { useTitleInfo } from "../../lib/useTitleInfo";
import { formatBytes, formatDuration } from "../../lib/format";
import { MAX_AUTO_RECOVER_ATTEMPTS } from "../../lib/uploadRecovery";
import { useTr } from "../../state/lang";
import { useConsoleLabel } from "../../state/roster";
import {
  installOrderPriority,
  useUploadQueueStore,
  type QueueItem,
  type QueueItemStatus,
} from "../../state/uploadQueue";
import { isRemotePath } from "../../lib/remotePath";
import { useTransferStore } from "../../state/transfer";
import { BottleneckLine, JobLiveNotes, UnsettledLine } from "./Bottleneck";
import { RarPasswordPrompt } from "./RarPasswordPrompt";
import { rarPasswordProblem } from "../../lib/rarPassword";

/** One console's slice of the queue, in first-seen order. */
interface ConsoleGroup {
  host: string;
  items: QueueItem[];
}

/** Partition the flat queue into per-console groups, preserving the order
 *  each console first appears so the layout is stable as items run/finish. */
function groupByConsole(items: QueueItem[]): ConsoleGroup[] {
  const groups: ConsoleGroup[] = [];
  const idx = new Map<string, number>();
  for (const it of items) {
    const h = hostOf(it.addr);
    let gi = idx.get(h);
    if (gi === undefined) {
      gi = groups.length;
      idx.set(h, gi);
      groups.push({ host: h, items: [] });
    }
    groups[gi].items.push(it);
  }
  return groups;
}

/** Inline queue panel rendered below the single-shot upload UI on the
 *  Upload screen. Visible only when the queue has items OR while the
 *  user is hydrating from disk so a perpetual blank slot doesn't waste
 *  space on first launch.
 *
 *  The queue is GROUPED BY CONSOLE: each PS5 with queued work gets its
 *  own collapsible section with its own Start/Stop, and the consoles
 *  upload in parallel. The top-level Start all / Stop all drives every
 *  console at once. This is what lets a queue holding games for 3
 *  different consoles actually upload to all 3 — and reorder one
 *  console's list while another console is mid-upload. */
/** The queue items to show: every console's, or just `host`'s. */
export function queueItemsForHost(
  items: QueueItem[],
  host: string | undefined,
): QueueItem[] {
  if (!host) return items;
  const h = hostOf(host);
  return items.filter((i) => hostOf(i.addr) === h);
}

/** Each pending item's 1-based place in its console's run order: the order the
 *  queue actually runs them in (base → update → DLC, then add order). */
export function pendingPositions(items: QueueItem[]): Map<string, number> {
  const pending = items
    .map((it, index) => ({ it, index }))
    .filter(({ it }) => it.status === "pending")
    .sort(
      (a, b) =>
        installOrderPriority(a.it) - installOrderPriority(b.it) ||
        a.index - b.index,
    );
  return new Map(pending.map(({ it }, i) => [it.id, i + 1]));
}

/** A console's rows in the order that answers "what's happening?": the one
 *  running now, then the waiting ones in the order they will run, then the
 *  finished ones — failures first (they want a decision), newest first. The
 *  underlying list keeps its own order; this is display only. Empty
 *  sections are left out. */
export function queueSections(
  items: QueueItem[],
): { key: "now" | "next" | "finished"; items: QueueItem[] }[] {
  const positions = pendingPositions(items);
  const now = items.filter((it) => it.status === "running");
  const next = items
    .filter((it) => it.status === "pending")
    .sort((a, b) => (positions.get(a.id) ?? 0) - (positions.get(b.id) ?? 0));
  const finished = items
    .filter((it) => it.status === "done" || it.status === "failed")
    .sort(
      (a, b) =>
        (a.status === "failed" ? 0 : 1) - (b.status === "failed" ? 0 : 1) ||
        (b.completedAt ?? 0) - (a.completedAt ?? 0),
    );
  const out: { key: "now" | "next" | "finished"; items: QueueItem[] }[] = [];
  if (now.length) out.push({ key: "now", items: now });
  if (next.length) out.push({ key: "next", items: next });
  if (finished.length) out.push({ key: "finished", items: finished });
  return out;
}

/** The console queue. On Upload it shows every console; Install Package
 *  passes `host` to show just the console being installed to. */
export function QueuePanel({ host }: { host?: string } = {}) {
  const tr = useTr();
  const allItems = useUploadQueueStore((s) => s.items);
  const items = useMemo(() => queueItemsForHost(allItems, host), [allItems, host]);
  const continueOnFailure = useUploadQueueStore((s) => s.continueOnFailure);
  const running = useUploadQueueStore((s) => s.running);
  const runningHosts = useUploadQueueStore((s) => s.runningHosts);
  const loaded = useUploadQueueStore((s) => s.loaded);
  const persistenceError = useUploadQueueStore((s) => s.persistenceError);
  const hydrate = useUploadQueueStore((s) => s.hydrate);
  const start = useUploadQueueStore((s) => s.start);
  const stop = useUploadQueueStore((s) => s.stop);
  const startHost = useUploadQueueStore((s) => s.startHost);
  const stopHost = useUploadQueueStore((s) => s.stopHost);
  const clear = useUploadQueueStore((s) => s.clear);
  const remove = useUploadQueueStore((s) => s.remove);
  const cancelItem = useUploadQueueStore((s) => s.cancelItem);
  const retryInstall = useUploadQueueStore((s) => s.retryInstall);
  const retryInstallViaUpload = useUploadQueueStore(
    (s) => s.retryInstallViaUpload,
  );
  const moveUp = useUploadQueueStore((s) => s.moveUp);
  const moveDown = useUploadQueueStore((s) => s.moveDown);
  const retryFailed = useUploadQueueStore((s) => s.retryFailed);
  const setContinueOnFailure = useUploadQueueStore(
    (s) => s.setContinueOnFailure,
  );
  // (2.11.0) Mutual-exclusion with the Upload-screen one-shot
  // transfer. The PS5 payload's transfer port is single-client,
  // so a queue Start while a one-shot is in flight would block at
  // the socket and the UI would show two "running" things. Gate
  // the Start buttons on transferInFlight; the Upload screen's
  // Upload button does the symmetric disable on `queueRunning`.
  // "Start all" stays conservatively gated on "any one-shot in flight" —
  // it would start EVERY console's drain loop, including the one whose
  // transfer port the one-shot currently owns. The per-console Start
  // buttons below gate on their OWN console's phase only (ConsoleGroup),
  // so console B's queue can start while console A runs a one-shot.
  const transferInFlight = useTransferStore((s) =>
    Object.values(s.phasesByHost).some(
      (p) => p.kind === "starting" || p.kind === "running",
    ),
  );

  // Hydrate once on mount. The store exposes a `loaded` flag so we
  // don't re-hydrate on screen re-mount; subsequent visits read from
  // the in-memory state set by the first hydrate.
  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  if (items.length === 0 && !persistenceError) return null;

  const pendingCount = items.filter((i) => i.status === "pending").length;
  const failedCount = items.filter((i) => i.status === "failed").length;
  const doneCount = items.filter((i) => i.status === "done").length;

  const groups = groupByConsole(items);
  // Single-console queues don't need a per-console sub-header — the
  // top-level Start already targets that one console. Only fan out the
  // grouped chrome once there's more than one console in play.
  const multiConsole = groups.length > 1;

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      {persistenceError && (
        <div className="mb-4">
          <ErrorCard
            title={tr(
              "save_failed",
              undefined,
              "Save failed",
            )}
            detail={persistenceError}
          />
        </div>
      )}
      <header className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2 text-sm font-semibold">
          <ListOrdered size={14} className="shrink-0" />
          <span className="whitespace-nowrap">
            {tr("queue_title", undefined, "Queue")}
          </span>
          {multiConsole && (
            <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs font-medium text-[var(--color-muted)]">
              {tr(
                "queue_console_total",
                { count: groups.length },
                `${groups.length} consoles`,
              )}
            </span>
          )}
          <QueueSummary
            running={items.length - pendingCount - failedCount - doneCount}
            pending={pendingCount}
            done={doneCount}
            failed={failedCount}
          />
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Toggle
            checked={continueOnFailure}
            onChange={setContinueOnFailure}
            label={tr("queue_continue_on_failure", undefined, "Continue on failure")}
          />

          {failedCount > 0 && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RotateCcw size={12} />}
              onClick={retryFailed}
            >
              {tr("queue_retry_failed", undefined, "Retry failed")}
            </Button>
          )}

          {running ? (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Square size={12} />}
              onClick={stop}
            >
              {multiConsole
                ? tr("queue_stop_all", undefined, "Stop all")
                : tr("queue_stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Play size={12} />}
              onClick={() => void start()}
              disabled={pendingCount === 0 || transferInFlight}
              title={
                transferInFlight
                  ? tr(
                      "queue_disabled_oneshot_in_flight",
                      undefined,
                      "A one-shot upload is in flight — wait for it to finish before starting the queue.",
                    )
                  : undefined
              }
            >
              {multiConsole
                ? tr("queue_start_all", undefined, "Start all")
                : tr("queue_start", undefined, "Start")}
            </Button>
          )}

          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Trash2 size={12} />}
            onClick={clear}
            disabled={running}
            title={tr(
              "queue_clear_tooltip",
              undefined,
              "Remove every item from the queue (including completed ones)",
            )}
          >
            {tr("queue_clear", undefined, "Clear all")}
          </Button>
        </div>
      </header>

      <div className="grid gap-4">
        {groups.map((g) => (
          <ConsoleGroup
            key={g.host}
            host={g.host}
            items={g.items}
            hostRunning={!!runningHosts[g.host]}
            showHeader={multiConsole}
            onStartHost={() => void startHost(g.host)}
            onStopHost={() => stopHost(g.host)}
            onMoveUp={moveUp}
            onMoveDown={moveDown}
            onRemove={remove}
            onCancel={cancelItem}
            onRetry={(id) => void retryInstall(id)}
            onRetryViaUpload={(id) => void retryInstallViaUpload(id)}
          />
        ))}
      </div>
    </section>
  );
}

/** The queue's counts as small chips — only the states that have any, so
 *  "2 total · 0 done · 1 pending · 0 failed" becomes "1 in progress · 1
 *  waiting". */
function QueueSummary({
  running,
  pending,
  done,
  failed,
}: {
  running: number;
  pending: number;
  done: number;
  failed: number;
}) {
  const tr = useTr();
  const chips: { key: string; text: string; tone: string }[] = [];
  if (running > 0)
    chips.push({
      key: "running",
      text: tr("queue_summary_running", { n: running }, `${running} in progress`),
      tone: "bg-[var(--color-accent)]/15 text-[var(--color-accent)]",
    });
  if (pending > 0)
    chips.push({
      key: "pending",
      text: tr("queue_summary_waiting", { n: pending }, `${pending} waiting`),
      tone: "bg-[var(--color-surface-3)] text-[var(--color-muted)]",
    });
  if (failed > 0)
    chips.push({
      key: "failed",
      text: tr("queue_summary_failed", { n: failed }, `${failed} failed`),
      tone: "bg-[var(--color-bad)]/15 text-[var(--color-bad)]",
    });
  if (done > 0)
    chips.push({
      key: "done",
      text: tr("queue_summary_done", { n: done }, `${done} done`),
      tone: "bg-[var(--color-good)]/15 text-[var(--color-good)]",
    });
  return (
    <span className="flex flex-wrap items-center gap-1.5">
      {chips.map((c) => (
        <span
          key={c.key}
          className={`rounded-full px-2 py-0.5 text-[11px] font-medium tabular-nums ${c.tone}`}
        >
          {c.text}
        </span>
      ))}
    </span>
  );
}

/** One console's section: a header naming the PS5 with its own Start/Stop
 *  + counts, then that console's queued rows. Collapsible so a queue with
 *  several consoles stays scannable. */
function ConsoleGroup({
  host,
  items,
  hostRunning,
  showHeader,
  onStartHost,
  onStopHost,
  onMoveUp,
  onMoveDown,
  onRemove,
  onCancel,
  onRetry,
  onRetryViaUpload,
}: {
  host: string;
  items: QueueItem[];
  hostRunning: boolean;
  showHeader: boolean;
  onStartHost: () => void;
  onStopHost: () => void;
  onMoveUp: (id: string) => void;
  onMoveDown: (id: string) => void;
  onRemove: (id: string) => void;
  onCancel: (id: string) => void;
  onRetry: (id: string) => void;
  onRetryViaUpload: (id: string) => void;
}) {
  const tr = useTr();
  const label = useConsoleLabel(host);
  const [collapsed, setCollapsed] = useState(false);
  // Gate THIS console's Start on THIS console's one-shot only. The transfer
  // port is single-client per PS5 — a one-shot on console A is irrelevant
  // to console B's queue, and gating on "any console" (the pre-2.31.0
  // behavior) wrongly blocked parallel multi-console operation.
  const transferInFlight = useTransferStore((s) => {
    const p = s.phasesByHost[host];
    return p?.kind === "starting" || p?.kind === "running";
  });

  // One pass instead of four .filter().length sweeps — this header
  // re-renders at progress-tick rate while uploads run, so the cost
  // (and the four throwaway arrays) repeats several times a second.
  const { runningN, pending, done, failed } = useMemo(() => {
    let runningN = 0;
    let pending = 0;
    let done = 0;
    let failed = 0;
    for (const i of items) {
      if (i.status === "running") runningN++;
      else if (i.status === "pending") pending++;
      else if (i.status === "done") done++;
      else if (i.status === "failed") failed++;
    }
    return { runningN, pending, done, failed };
  }, [items]);

  const positions = useMemo(() => pendingPositions(items), [items]);
  const sections = useMemo(() => queueSections(items), [items]);
  // A waiting row can move only past a neighbour in the same install tier —
  // the tier (base → update → DLC) decides the order before the list does.
  const movable = useMemo(() => {
    const next = sections.find((sec) => sec.key === "next")?.items ?? [];
    const m = new Map<string, { up: boolean; down: boolean }>();
    next.forEach((it, i) => {
      const tier = installOrderPriority(it);
      m.set(it.id, {
        up: i > 0 && installOrderPriority(next[i - 1]) === tier,
        down: i < next.length - 1 && installOrderPriority(next[i + 1]) === tier,
      });
    });
    return m;
  }, [sections]);
  // Headings only earn their space once the list mixes states; a queue that
  // is all waiting (or all finished) reads fine without one.
  const labelled = sections.length > 1;
  const rows = (
    <div className="grid gap-3">
      {sections.map((section) => (
        <div key={section.key}>
          {labelled && (
            <div className="mb-1.5 flex items-center gap-2 text-[11px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
              <span>
                {section.key === "now"
                  ? tr("queue_section_now", undefined, "Now")
                  : section.key === "next"
                    ? tr("queue_section_next", undefined, "Up next")
                    : tr("queue_section_finished", undefined, "Finished")}
              </span>
              <span className="rounded-full bg-[var(--color-surface-3)] px-1.5 font-mono text-[10px] tabular-nums">
                {section.items.length}
              </span>
            </div>
          )}
          <ul className="grid gap-1.5">
            {section.items.map((item) => {
              const position = positions.get(item.id);
              return (
                <QueueRow
                  key={item.id}
                  item={item}
                  position={position}
                  canMoveUp={movable.get(item.id)?.up ?? false}
                  canMoveDown={movable.get(item.id)?.down ?? false}
                  onMoveUp={() => onMoveUp(item.id)}
                  onMoveDown={() => onMoveDown(item.id)}
                  onRemove={() => onRemove(item.id)}
                  onCancel={() => onCancel(item.id)}
                  onRetry={() => onRetry(item.id)}
                  onRetryViaUpload={() => onRetryViaUpload(item.id)}
                />
              );
            })}
          </ul>
        </div>
      ))}
    </div>
  );

  // Single-console queue: render the rows bare (the top-level header
  // already names the only console in play).
  if (!showHeader) return rows;

  return (
    <div
      className={`rounded-md border ${
        hostRunning
          ? "border-[var(--color-accent)]"
          : "border-[var(--color-border)]"
      } bg-[var(--color-surface)]`}
    >
      <div className="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => setCollapsed((c) => !c)}
          className="flex min-w-0 items-center gap-2 rounded-sm px-1 py-0.5 text-left transition-colors hover:bg-[var(--color-surface-3)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
          title={
            collapsed
              ? tr("queue_group_expand", undefined, "Show this console's queue")
              : tr(
                  "queue_group_collapse",
                  undefined,
                  "Hide this console's queue",
                )
          }
        >
          <ChevronRight
            size={14}
            className={`shrink-0 text-[var(--color-muted)] transition-transform ${
              collapsed ? "" : "rotate-90"
            }`}
          />
          <span aria-hidden>🖥</span>
          <span className="truncate text-sm font-semibold">{label}</span>
          <span className="shrink-0 font-mono text-xs text-[var(--color-muted)]">
            {host}
          </span>
        </button>

        <div className="flex items-center gap-3">
          <span className="text-xs text-[var(--color-muted)]">
            {tr(
              "queue_group_summary",
              { running: runningN, pending, done, failed },
              `${runningN} uploading · ${pending} queued · ${done} done${
                failed ? ` · ${failed} failed` : ""
              }`,
            )}
          </span>
          {hostRunning ? (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Square size={12} />}
              onClick={onStopHost}
            >
              {tr("queue_stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Play size={12} />}
              onClick={onStartHost}
              disabled={pending === 0 || transferInFlight}
              title={
                transferInFlight
                  ? tr(
                      "queue_disabled_oneshot_in_flight",
                      undefined,
                      "A one-shot upload is in flight — wait for it to finish before starting the queue.",
                    )
                  : pending === 0
                    ? tr(
                        "queue_group_nothing_pending",
                        undefined,
                        "Nothing queued for this console",
                      )
                    : undefined
              }
            >
              {tr("queue_start", undefined, "Start")}
            </Button>
          )}
        </div>
      </div>

      {!collapsed && <div className="px-3 pb-3">{rows}</div>}
    </div>
  );
}

export function QueueRow({
  item,
  position,
  canMoveUp = true,
  canMoveDown = true,
  onMoveUp,
  onMoveDown,
  onRemove,
  onCancel,
  onRetry,
  onRetryViaUpload,
}: {
  item: QueueItem;
  /** 1-based place in this console's run order, for a waiting item. */
  position?: number;
  /** Whether a waiting row has a neighbour in the run order to swap with. */
  canMoveUp?: boolean;
  canMoveDown?: boolean;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onRemove: () => void;
  onCancel: () => void;
  onRetry: () => void;
  onRetryViaUpload: () => void;
}) {
  const tr = useTr();
  const isInstall = item.sourceKind === "install";
  const viewPath = queueItemViewPath(item);
  // Game identity for the row — so you can tell what's what at a glance.
  // pkg: title id parsed out of the ContentID drives the cover (appmeta/CDN)
  // and the PS4/PS5 badge. game-folder: the folder's own sce_sys/icon0.png.
  // Other kinds (plain file, archive, image) have no game art, so no thumb.
  const titleId =
    item.sourceKind === "pkg" || isInstall
      ? titleIdFromContentId(item.contentId)
      : null;
  const titleInfo = useTitleInfo(titleId);
  const platform = platformForTitleId(titleId);
  const hasGameArt =
    item.sourceKind === "pkg" ||
    item.sourceKind === "game-folder" ||
    isInstall;
  const rowName = queueRowName(item, titleInfo?.title);
  const pct =
    item.totalBytes > 0
      ? Math.max(0, Math.min(100, (item.bytesSent / item.totalBytes) * 100))
      : 0;
  const isActive = item.status === "running";
  const isPending = item.status === "pending";
  // Between auto-recovery attempts: the prior attempt failed for a
  // recoverable reason and the runner is waiting out a backoff / re-deploying
  // the payload before resuming. Status is still "running" so the row reads as
  // in-flight, but we swap the (now-zeroed) progress bar for a recovery banner.
  const isRecovering = isActive && !!item.recovering;
  // Finalize phase: all shards on the wire, engine waiting on PS5
  // commit. Drives a different chip + suppresses the stale ETA — see
  // the speed/eta render below. Gate on totalBytes > 0 so a row whose
  // stat is still pending (totalBytes === 0 on the first tick) doesn't
  // false-positive as finalized.
  const phase = rowPhase(item);
  const isFinalizing = phase === "finalizing";
  // An install item, or a staged .pkg whose upload finished and is now
  // installing: both show the install's own progress block, so everything
  // about the running install — phase, bytes, speed, time left, what it is
  // waiting on — lives in this row rather than in a banner elsewhere.
  const showInstallProgress =
    isActive && !isRecovering && (isInstall || phase === "installing");
  // Show ETA only when we have a real total + a real rate; otherwise
  // the readout would print "ETA Infinity" or "ETA 0s" right at the
  // start of a transfer where the smoother hasn't seen two samples yet.
  const remainingBytes = Math.max(0, item.totalBytes - item.bytesSent);
  const etaSec =
    item.bytesPerSec > 0 && remainingBytes > 0
      ? remainingBytes / item.bytesPerSec
      : null;
  const kind = packageKindLabel(item.category, tr);
  const finished = item.status === "done" || item.status === "failed";

  return (
    <li
      className={`rounded-md border text-sm transition-colors ${
        isActive ? "p-3" : "px-3 py-2"
      } ${
        item.status === "failed"
          ? "border-[var(--color-bad)] bg-[var(--color-surface)]"
          : isActive
            ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
            : "border-[var(--color-border)] bg-[var(--color-surface)]"
      }`}
    >
      <div className="flex items-center gap-3">
        {isPending && position != null ? (
          <span
            className="flex h-[18px] min-w-[18px] shrink-0 items-center justify-center rounded-full bg-[var(--color-surface-3)] px-1 text-[10px] font-semibold tabular-nums text-[var(--color-muted)]"
            title={tr(
              "queue_install_waiting",
              { n: position },
              `Queued (#${position})`,
            )}
          >
            <span aria-hidden>{position}</span>
            <span className="sr-only">
              {tr(
                "queue_install_waiting",
                { n: position },
                `Queued (#${position})`,
              )}
            </span>
          </span>
        ) : (
          <StatusIcon status={item.status} />
        )}
        {hasGameArt && (
          // A phone gives the title the room a small cover would take; the
          // running row keeps its cover — it is the one being watched.
          <div className={isActive ? "shrink-0" : "hidden shrink-0 sm:block"}>
            <GameIcon
              host={hostOf(item.addr)}
              size={isActive ? 44 : 32}
              titleId={titleId}
              gamePath={
                item.sourceKind === "game-folder" ? item.sourcePath : null
              }
              fallbackSrc={titleInfo?.coverImageUrl ?? null}
            />
          </div>
        )}
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
            {/* Two lines at most, broken anywhere: a raw content id has no
                spaces, and a one-line ellipsis left "H…" on a phone. */}
            <span className="line-clamp-2 min-w-0 font-medium [overflow-wrap:anywhere]">
              {rowName}
            </span>
            {platform && <PlatformBadge platform={platform} />}
            {kind && (
              <span className="shrink-0 rounded bg-[var(--color-surface-3)] px-1.5 py-px text-[10px] font-medium uppercase tracking-wide text-[var(--color-muted)]">
                {kind}
              </span>
            )}
          </div>
          <div className="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1 gap-y-0.5 text-xs text-[var(--color-muted)]">
            {isInstall ? (
              <span>{installSourceLabel(item, tr)}</span>
            ) : (
              <span className="truncate font-mono">→ {item.resolvedDest}</span>
            )}
            {!isInstall && !finished && (
              <span>
                ·{" "}
                {tr(
                  `queue_strategy_${item.strategy}`,
                  undefined,
                  item.strategy === "resume" ? "Resume" : "Overwrite",
                )}
              </span>
            )}
            {item.excludes.length > 0 && (
              <span>
                ·{" "}
                {tr(
                  "queue_excludes",
                  { count: item.excludes.length },
                  `${item.excludes.length} exclude${
                    item.excludes.length === 1 ? "" : "s"
                  }`,
                )}
              </span>
            )}
            {item.mountAfterUpload && !item.mountedAt && (
              <span>
                · {tr("queue_will_mount", undefined, "mount after upload")}
              </span>
            )}
            {item.mountedAt && (
              <span className="font-mono text-[var(--color-accent)]">
                ·{" "}
                {tr(
                  "queue_mounted_at",
                  { mount: item.mountedAt },
                  "mounted at {mount}",
                )}
              </span>
            )}
            {item.sourceKind === "pkg" &&
              item.installAfterUpload !== false &&
              !item.installPhase && (
                <span>
                  · {tr("queue_will_install", undefined, "install after upload")}
                </span>
              )}
            {(item.installPhase === "done" ||
              item.installPhase === "warn" ||
              item.installPhase === "unverified") && (
              <span
                className={
                  item.installPhase === "warn" ||
                  item.installPhase === "unverified"
                    ? "text-[var(--color-warn)]"
                    : "text-[var(--color-good)]"
                }
              >
                ·{" "}
                {item.installPhase === "unverified"
                  ? tr(
                      "queue_install_unverified",
                      undefined,
                      "install accepted; verify on PS5 (package kept)",
                    )
                  : item.installPhase === "warn"
                  ? tr(
                      "queue_installed_warn",
                      undefined,
                      "installed (may not launch)",
                    )
                  : tr("queue_installed", undefined, "installed")}
                {item.installProgress && item.installProgress.total > 0
                  ? ` · ${formatBytes(item.installProgress.total)}`
                  : ""}
              </span>
            )}
          </div>
        </div>

        {/* The live percentage sits at the row's right edge, where the eye
            lands when scanning a list of installs. */}
        {showInstallProgress && item.installProgress && (
          <span className="hidden shrink-0 text-lg font-semibold sm:inline tabular-nums text-[var(--color-accent)]">
            {installPctOf(item)}%
          </span>
        )}

        <div className="flex shrink-0 items-center gap-0.5">
          {isPending && (
            <>
              <button
                type="button"
                onClick={onMoveUp}
                disabled={!canMoveUp}
                title={tr("queue_move_up", undefined, "Move up")}
                aria-label={tr("queue_move_up", undefined, "Move up")}
                className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
              >
                <ArrowUp size={14} />
              </button>
              <button
                type="button"
                onClick={onMoveDown}
                disabled={!canMoveDown}
                title={tr("queue_move_down", undefined, "Move down")}
                aria-label={tr("queue_move_down", undefined, "Move down")}
                className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
              >
                <ArrowDown size={14} />
              </button>
            </>
          )}
          {viewPath && (
            <button
              type="button"
              onClick={() => usePackageViewer.getState().open(viewPath)}
              title={tr("viewer_open", undefined, "View details")}
              aria-label={tr("viewer_open", undefined, "View details")}
              className={`rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] ${
                isActive ? "" : "hidden sm:block"
              }`}
            >
              <ScanSearch size={14} />
            </button>
          )}
          {isActive && isInstall ? null : isActive ? (
            // An install can't be stopped halfway (Sony's installer owns it
            // once it starts), so a running install row has no Cancel.
            // The actively-uploading row: move/remove are locked (mutating the
            // array under the runner is unsafe), so Cancel is the only per-item
            // control here. It aborts THIS upload (partial transfer stays
            // resumable) and lets the console's other pending jobs keep going —
            // unlike Stop, which halts the whole console.
            <button
              type="button"
              onClick={onCancel}
              title={tr(
                "queue_cancel_item",
                undefined,
                "Cancel this upload (keeps the rest of the queue going)",
              )}
              className="rounded p-1 text-[var(--color-bad)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)]"
            >
              <Ban size={14} />
            </button>
          ) : (
            <button
              type="button"
              onClick={onRemove}
              title={tr("queue_remove", undefined, "Remove from queue")}
              aria-label={tr("queue_remove", undefined, "Remove from queue")}
              className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)]"
            >
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      {isRecovering && (
        <div className="mt-2 flex items-center gap-2 rounded-md bg-[var(--color-warn)]/10 px-2 py-1.5 text-xs text-[var(--color-warn)]">
          <Spinner size={12} className="shrink-0" />
          <span>
            {tr(
              "queue_recovering",
              {
                attempt: item.recoverAttempt ?? 1,
                max: MAX_AUTO_RECOVER_ATTEMPTS,
              },
              `Connection lost — re-deploying payload & resuming (${
                item.recoverAttempt ?? 1
              }/${MAX_AUTO_RECOVER_ATTEMPTS})…`,
            )}
          </span>
        </div>
      )}

      {showInstallProgress && <InstallProgressBlock item={item} />}

      {isActive && !showInstallProgress && !isRecovering && (
        <div className="mt-2">
          <div className="mb-1 flex flex-wrap items-baseline justify-between gap-x-3 text-xs text-[var(--color-muted)]">
            <span>
              {formatBytes(item.bytesSent)} / {formatBytes(item.totalBytes)}
              {/* Speed + ETA are honest signals only while bytes are
                  still moving. Once the row pegs at 100%, the engine
                  is waiting on the PS5's commit (drain ACKs + COMMIT
                  ACK across potentially tens-of-thousands of inodes)
                  and the saved bytesPerSec is a stale figure that no
                  longer reflects reality — same pattern as the
                  single-shot Upload banner. Replace with a
                  Finalizing… chip instead. */}
              {!isFinalizing && item.bytesPerSec > 0 && (
                <>
                  {" · "}
                  <span className="tabular-nums">
                    {formatBytes(item.bytesPerSec)}/s
                  </span>
                  {etaSec !== null && (
                    <>
                      {" · "}
                      <span className="tabular-nums">
                        {tr(
                          "queue_eta",
                          { eta: formatDuration(etaSec) },
                          "ETA {eta}",
                        )}
                      </span>
                    </>
                  )}
                </>
              )}
              {isFinalizing && (
                <>
                  {" · "}
                  <span
                    className="rounded-full bg-[var(--color-warn)]/15 px-1.5 py-0.5 text-xs font-medium text-[var(--color-warn)]"
                    title={tr(
                      "queue_phase_finalizing_hint",
                      undefined,
                      "All bytes are on the PS5; it's committing the file index. Large file counts (10k+) routinely take many minutes here — don't close the app.",
                    )}
                  >
                    {item.filesFinalizingTotal > 0
                      ? tr(
                          "queue_phase_finalizing_with_counter",
                          {
                            done: item.filesFinalized.toLocaleString(),
                            total: item.filesFinalizingTotal.toLocaleString(),
                          },
                          `Finalizing on PS5 — ${item.filesFinalized.toLocaleString()} / ${item.filesFinalizingTotal.toLocaleString()}`,
                        )
                      : tr(
                          "queue_phase_finalizing",
                          undefined,
                          "Finalizing on PS5",
                        )}
                  </span>
                </>
              )}
            </span>
            <span className="tabular-nums">{pct.toFixed(0)}%</span>
          </div>
          <ProgressBar
            value={pct / 100}
            label={tr("queue_title", undefined, "Queue")}
          />
          <div className="mt-1">
            <JobLiveNotes live={item.live} />
          </div>
          {isFinalizing && (
            // Always-visible explainer under the bar. The pill itself
            // ("Finalizing on PS5") is short enough to fit on the
            // progress line, but the actionable "don't close the app"
            // sentence is what actually prevents the force-quit that
            // started this whole bug. Tooltip-hidden hints don't get
            // read on a Tauri desktop app — promote it.
            <div className="mt-1 text-xs text-[var(--color-warn)]">
              {tr(
                "queue_phase_finalizing_hint",
                undefined,
                "All bytes are on the PS5; it's committing the file index. Large file counts (10k+) routinely take many minutes here — don't close the app.",
              )}
            </div>
          )}
        </div>
      )}

      {item.status === "done" && !isInstall && (
        <DoneStats bytesSent={item.bytesSent} bytesPerSec={item.bytesPerSec} />
      )}
      {item.status === "done" && !isInstall && item.live?.bottleneck && (
        <BottleneckLine cause={item.live.bottleneck} />
      )}
      {item.status === "done" && !isInstall && <UnsettledLine live={item.live} />}

      {item.status === "done" && item.installNote && (
        <div className="mt-1 text-xs text-[var(--color-muted)]">
          {item.installNote}
        </div>
      )}

      {item.status === "failed" && item.error && (
        <FailedRowErrorCard
          rawError={item.error}
          reason={item.errorReason}
          detail={item.errorDetail}
        />
      )}

      {item.status === "failed" &&
        !isInstall &&
        rarPasswordProblem(item.errorReason, item.error) && (
          <RarPasswordPrompt
            problem={rarPasswordProblem(item.errorReason, item.error)!}
            onSubmit={(pw) =>
              useUploadQueueStore.getState().retryWithPassword(item.id, pw)
            }
          />
        )}

      {item.status === "failed" && isInstall && (
        <div className="mt-2 flex flex-wrap gap-2">
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RotateCcw size={12} />}
            onClick={onRetry}
          >
            {tr("task_retry", undefined, "Retry")}
          </Button>
          {item.fallbackToUpload && (
            <Button
              variant="primary"
              size="sm"
              leftIcon={<UploadCloud size={12} />}
              onClick={onRetryViaUpload}
            >
              {tr("queue_retry_via_upload", undefined, "Retry via upload")}
            </Button>
          )}
        </div>
      )}
    </li>
  );
}

/** A running install's percentage, clamped: 100 is reserved for "done". */
function installPctOf(item: QueueItem): number {
  return typeof item.installPct === "number"
    ? Math.max(0, Math.min(99, item.installPct))
    : 0;
}

/** Base game / Update / DLC, from the PARAM.SFO category. */
function packageKindLabel(
  category: string | null | undefined,
  tr: ReturnType<typeof useTr>,
): string | null {
  if (category === "gp") return tr("queue_kind_update", undefined, "Update");
  if (category === "ac") return tr("queue_kind_dlc", undefined, "DLC");
  return null;
}

/** The running install's phase, progress and numbers, all in one place:
 *
 *    Installing on the PS5
 *    ████████░░░░░░░░░░░░░░░░░░░░░░░
 *    13.86 GB of 101.91 GB          120 MB/s · 12 min left
 *    ⓘ Verifying the exact installed package on the PS5…
 *
 *  Before the first numbers arrive the bar sweeps and the label says it is
 *  getting ready, so a slow start never reads as a frozen 0%. */
export function InstallProgressBlock({ item }: { item: QueueItem }) {
  const tr = useTr();
  const p = item.installProgress ?? null;
  const phaseLabel = !p
    ? tr("queue_phase_preparing", undefined, "Getting ready…")
    : p.phase === "stage"
      ? tr("queue_phase_stage", undefined, "Copying to internal storage")
      : p.phase === "transfer"
        ? tr("queue_phase_transfer", undefined, "Sending to the PS5")
        : tr("queue_phase_install", undefined, "Installing on the PS5");
  const remaining = p ? Math.max(0, p.total - p.current) : 0;
  const eta =
    p && p.bytesPerSec > 0 && remaining > 0
      ? formatDuration(remaining / p.bytesPerSec)
      : null;
  const rate =
    p && p.originBytesPerSec && p.originBytesPerSec > 0
      ? tr(
          "queue_link_rates",
          {
            down: formatBytes(p.originBytesPerSec),
            up: formatBytes(p.bytesPerSec),
          },
          `downloading ${formatBytes(p.originBytesPerSec)}/s · sending ${formatBytes(p.bytesPerSec)}/s`,
        )
      : p && p.bytesPerSec > 0
        ? `${formatBytes(p.bytesPerSec)}/s`
        : null;
  return (
    <div className="mt-3">
      <div className="mb-1.5 flex items-baseline justify-between gap-3 text-xs font-medium text-[var(--color-text)]">
        <span>{phaseLabel}</span>
        {/* On a phone the percentage moves here from the row's right edge,
            which it would take from the title. */}
        {p && (
          <span className="tabular-nums text-[var(--color-accent)] sm:hidden">
            {installPctOf(item)}%
          </span>
        )}
      </div>
      <ProgressBar
        value={p ? installPctOf(item) / 100 : null}
        label={phaseLabel}
      />
      {p && (
        <div className="mt-1.5 flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5 text-xs tabular-nums text-[var(--color-muted)]">
          <span>
            {tr(
              "queue_bytes_of",
              { done: formatBytes(p.current), total: formatBytes(p.total) },
              `${formatBytes(p.current)} of ${formatBytes(p.total)}`,
            )}
          </span>
          {(rate || eta) && (
            <span>
              {rate}
              {rate && eta ? " · " : ""}
              {eta &&
                tr("queue_time_left", { eta }, `${eta} left`)}
            </span>
          )}
        </div>
      )}
      {item.installNote && (
        <div className="mt-2 flex items-start gap-1.5 text-xs text-[var(--color-muted)]">
          <Info size={12} className="mt-px shrink-0" />
          <span className="min-w-0 break-words">{item.installNote}</span>
        </div>
      )}
    </div>
  );
}

/** Layered error card: humanized hint first (if we recognize the
 *  payload's `error_reason`), then the payload's `detail` string,
 *  then a collapsed `<details>` carrying the raw error chain for
 *  power-user debugging. Falls back to plain raw-error rendering when
 *  no structured fields are present (engine-internal failures, older
 *  payloads). */
/** What a live queue row is doing once its bytes are all sent: committing
 *  the upload ("finalizing") or — for a pkg — installing it. Without the
 *  install phase the row kept showing the upload's "committing the file
 *  index… don't close the app" for the whole install. */
export function rowPhase(item: {
  status: string;
  totalBytes: number;
  bytesSent: number;
  installPhase?: string | null;
}): "installing" | "finalizing" | null {
  if (item.status !== "running") return null;
  if (item.installPhase === "installing") return "installing";
  if (item.totalBytes > 0 && item.bytesSent >= item.totalBytes) return "finalizing";
  return null;
}

/** A queue row's display name. For a pkg, the resolved/online title wins,
 *  then the finisher's installed title, then the file name. The finisher can
 *  store a raw content id as `installedTitle`, so preferring it made a row
 *  flip from "Star Wars…" to "UP1082-…" the moment its install finished. */
export function queueRowName(
  item: { sourceKind: string; installedTitle?: string | null; displayName: string },
  onlineTitle: string | undefined,
): string {
  if (item.sourceKind === "pkg") {
    return onlineTitle || item.installedTitle || item.displayName;
  }
  return item.displayName;
}

/** Size + average speed on a finished queue row. `speed` is the bare size:
 *  every locale's `queue_avg_speed` template supplies the "/s" itself. */
export function DoneStats({
  bytesSent,
  bytesPerSec,
}: {
  bytesSent: number;
  bytesPerSec: number;
}) {
  const tr = useTr();
  if (!(bytesPerSec > 0)) return null;
  return (
    <div className="mt-2 text-xs text-[var(--color-muted)]">
      {formatBytes(bytesSent)}
      {" · "}
      <span className="tabular-nums">
        {tr(
          "queue_avg_speed",
          { speed: formatBytes(bytesPerSec) },
          "{speed}/s avg",
        )}
      </span>
    </div>
  );
}

function FailedRowErrorCard({
  rawError,
  reason,
  detail,
}: {
  rawError: string;
  reason: string | null;
  detail: string | null;
}) {
  const tr = useTr();
  const humanized = humanizeJobErrorReason(reason ?? undefined);
  return (
    <div className="mt-2">
      <ErrorCard
        title={humanized ?? tr("queue_error", "Error")}
        detail={
          humanized ? (
            <>
              {reason && (
                <div
                  className="mt-0.5 font-mono text-[10px] text-[var(--color-muted)]"
                  data-testid="error-reason-code"
                >
                  {reason}
                </div>
              )}
              {detail && (
                <div className="mt-1 text-xs text-[var(--color-muted)]">
                  {detail}
                </div>
              )}
              <details className="mt-1 cursor-pointer">
                <summary className="text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]">
                  {tr("queue_raw_error", "raw error")}
                </summary>
                <code className="mt-1 block whitespace-pre-wrap break-all font-mono text-xs text-[var(--color-muted)]">
                  {rawError}
                  {reason && `\n[reason: ${reason}]`}
                </code>
              </details>
            </>
          ) : (
            <code className="block whitespace-pre-wrap break-all font-mono text-xs">
              {rawError}
            </code>
          )
        }
      />
    </div>
  );
}

function StatusIcon({ status }: { status: QueueItemStatus }) {
  switch (status) {
    case "pending":
      return (
        <CircleDashed
          size={18}
          className="mt-0.5 shrink-0 text-[var(--color-muted)]"
        />
      );
    case "running":
      return (
        <Spinner
          size={18}
          tone="accent"
          className="mt-0.5 shrink-0"
        />
      );
    case "done":
      return (
        <CheckCircle2
          size={18}
          className="mt-0.5 shrink-0 text-[var(--color-good)]"
        />
      );
    case "failed":
      return (
        <XCircle
          size={18}
          className="mt-0.5 shrink-0 text-[var(--color-bad)]"
        />
      );
  }
}

/** Where an install item's package comes from, in words. */
function installSourceLabel(
  item: QueueItem,
  tr: ReturnType<typeof useTr>,
): string {
  const req = item.install;
  if (req?.via === "stream" && isRemotePath(req.source)) {
    return tr("queue_install_from_server", undefined, "Stream install from a saved server");
  }
  if (req?.via === "stream") {
    return tr("queue_install_from_pc", undefined, "Stream install from this computer");
  }
  if (req?.via === "link" || item.sourcePath.startsWith("url:")) {
    return tr("queue_install_from_link", undefined, "Install from a link");
  }
  return tr("queue_install_on_ps5", undefined, "Install from the PS5");
}
