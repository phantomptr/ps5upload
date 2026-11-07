import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { Eraser, File, Folder, FolderOpen, RefreshCw, Square, Trash2, Wand2 } from "lucide-react";

import {
  cleanupApply,
  cleanupApplyStatus,
  cleanupCancel,
  cleanupScanStart,
  cleanupScanStatus,
  CleanupError,
  type CleanupItem,
  type CleanupScan,
} from "../../api/cleanup";
import {
  Badge,
  Button,
  Callout,
  ConnectionGate,
  EmptyState,
  GameIcon,
  Modal,
  PageHeader,
  Spinner,
} from "../../components";
import {
  baseName,
  groupItems,
  groupState,
  initialSelection,
  isRemovable,
  itemBytes,
  parentDir,
  selectionTotals,
  setGroup,
  toggle,
  type ApplySummary,
  type CleanupGroup,
} from "../../lib/cleanupModel";
import {
  categoryHint,
  categoryTitle,
  driveName,
  itemDetails,
  phaseText,
  reasonText,
  refusalText,
  selectionLine,
  type CleanupT,
} from "../../lib/cleanupText";
import { formatBytes } from "../../lib/format";
import { saveFsLastPath } from "../../lib/fsLastPath";
import { parseSpacePlanQuery } from "../../lib/spacePlan";
import { useConnectionStore } from "../../state/connection";
import {
  attachRunningApply,
  cleanupRunOf,
  dismissCleanupResult,
  followApply,
  HELPER_SILENT,
  startedApply,
  STOP_UNCONFIRMED,
  stopCleanupApply,
  useCleanupRuns,
} from "../../state/cleanupRun";
import { useTr } from "../../state/lang";
import { SpacePlanner } from "./SpacePlanner";
import { NotPlayedCard } from "./NotPlayedCard";


const SCAN_POLL_MS = 700;

function errorText(t: CleanupT, e: unknown): string {
  if (e instanceof CleanupError) {
    // #714: the helper died or the console went away while the PS5 was removing an update.
    if (e.reason === HELPER_SILENT) return t("cleanup_err_helper_silent");
    if (e.reason === STOP_UNCONFIRMED) return t("cleanup_err_stop_unconfirmed");
    const known = refusalText(t, e.reason);
    if (known) return known;
  }
  return e instanceof Error ? e.message : String(e);
}

function itemTitle(item: CleanupItem): string {
  if (item.cat === "C" || item.cat === "D" || item.cat === "G") return item.name ? `${item.name}` : baseName(item.path);
  return item.name || item.title_id || baseName(item.path);
}

function ItemRow({
  host,
  item,
  checked,
  disabled,
  onToggle,
  onShow,
}: {
  host: string;
  item: CleanupItem;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
  onShow: () => void;
}) {
  const tr = useTr();
  const removable = isRemovable(item);
  const title = itemTitle(item);
  return (
    <li className="flex items-center gap-3 px-3 py-2" data-testid={`cleanup-item-${item.id}`}>
      {removable ? (
        <input
          type="checkbox"
          className="checkbox h-5 w-5 shrink-0"
          checked={checked}
          disabled={disabled}
          onChange={onToggle}
          aria-label={title}
        />
      ) : (
        <span className="h-5 w-5 shrink-0" aria-hidden />
      )}
      {item.title_id ? (
        <GameIcon host={host} titleId={item.title_id} size={36} />
      ) : (
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-[var(--color-surface-3)] text-[var(--color-muted)]">
          {item.is_dir ? <Folder size={16} /> : <File size={16} />}
        </span>
      )}
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
          <span className="truncate">{title}</span>
          {item.title_id && title !== item.title_id ? (
            <span className="font-mono text-xs text-[var(--color-muted)]">{item.title_id}</span>
          ) : null}
          <Badge size="sm">{driveName(tr, item.drive)}</Badge>
        </div>
        <div className="text-xs text-[var(--color-muted)]">{itemDetails(tr, item)}</div>
        <div className="truncate font-mono text-[11px] text-[var(--color-muted)]" title={item.path}>
          {item.path}
        </div>
      </div>
      <div className="shrink-0 text-end text-sm tabular-nums">{formatBytes(itemBytes(item))}</div>
      {item.cat === "G" ? (
        <Button size="sm" variant="ghost" leftIcon={<FolderOpen size={14} />} onClick={onShow}>
          {tr("cw_cleanup_show_in_files", undefined, "Show in Files")}
        </Button>
      ) : null}
    </li>
  );
}

function GroupCard({
  host,
  group,
  selected,
  disabled,
  onSelect,
  onShow,
}: {
  host: string;
  group: CleanupGroup;
  selected: Set<number>;
  disabled: boolean;
  onSelect: (next: Set<number>) => void;
  onShow: (item: CleanupItem) => void;
}) {
  const tr = useTr();
  const st = groupState(group, selected);
  const anyRemovable = group.items.some(isRemovable);
  return (
    <section className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)]" data-testid={`cleanup-group-${group.cat}`}>
      <header className="flex flex-wrap items-center gap-3 border-b border-[var(--color-border)] px-3 py-2">
        {anyRemovable ? (
          <input
            type="checkbox"
            className="checkbox h-5 w-5 shrink-0"
            checked={st === "all"}
            ref={(el) => {
              if (el) el.indeterminate = st === "some";
            }}
            disabled={disabled}
            onChange={() => onSelect(setGroup(group, selected, st !== "all"))}
            aria-label={tr("cw_cleanup_select_group", { group: categoryTitle(tr, group.cat) }, `Select all: ${categoryTitle(tr, group.cat)}`)}
          />
        ) : null}
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">
            {categoryTitle(tr, group.cat)}
            <span className="ms-2 text-xs font-normal text-[var(--color-muted)]">
              {tr("cw_cleanup_group_count", { count: group.items.length }, `${group.items.length} items`)}
            </span>
          </div>
          <div className="text-xs text-[var(--color-muted)]">{categoryHint(tr, group.cat)}</div>
        </div>
        <div className="shrink-0 text-sm font-semibold tabular-nums">{formatBytes(group.bytes)}</div>
      </header>
      <ul className="divide-y divide-[var(--color-border)]">
        {group.items.map((i) => (
          <ItemRow
            key={i.id}
            host={host}
            item={i}
            checked={selected.has(i.id)}
            disabled={disabled}
            onToggle={() => onSelect(toggle(selected, i.id))}
            onShow={() => onShow(i)}
          />
        ))}
      </ul>
    </section>
  );
}

function ConfirmRemove({
  items,
  selected,
  onCancel,
  onConfirm,
}: {
  items: CleanupItem[];
  selected: Set<number>;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const tr = useTr();
  const totals = selectionTotals(items, selected);
  const chosen = items.filter((i) => selected.has(i.id) && isRemovable(i));
  const shown = chosen.slice(0, 12);
  return (
    <Modal
      open
      onClose={onCancel}
      title={tr("cw_cleanup_confirm_title", { count: totals.count }, `Remove ${totals.count} items from the PS5?`)}
      size="lg"
      footer={
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="danger" leftIcon={<Trash2 size={14} />} onClick={onConfirm} data-testid="cleanup-confirm">
            {tr("cw_cleanup_remove_size", { size: formatBytes(totals.bytes) }, `Remove ${formatBytes(totals.bytes)}`)}
          </Button>
        </div>
      }
    >
      <div className="space-y-3 text-sm">
        <ul className="max-h-60 space-y-1 overflow-auto">
          {shown.map((i) => (
            <li key={i.id} className="flex justify-between gap-3">
              <span className="truncate">
                {itemTitle(i)} <span className="text-xs text-[var(--color-muted)]">· {categoryTitle(tr, i.cat)}</span>
              </span>
              <span className="shrink-0 tabular-nums text-[var(--color-muted)]">{formatBytes(itemBytes(i))}</span>
            </li>
          ))}
          {chosen.length > shown.length ? (
            <li className="text-xs text-[var(--color-muted)]">
              {tr("cw_cleanup_and_more", { count: chosen.length - shown.length }, `and ${chosen.length - shown.length} more`)}
            </li>
          ) : null}
        </ul>
        {totals.downgrades.length > 0 ? (
          <Callout tone="warn" title={tr("cw_cleanup_downgrade_title", undefined, "Removing updates")}>
            {tr(
              "cw_cleanup_downgrade_body",
              undefined,
              "These games go back to their base version. Online play may ask for the update again. Saves stay, but a save made with the newer version may not load on the older one. You can install the update again later.",
            )}
          </Callout>
        ) : null}
        <p className="text-[var(--color-muted)]">
          {tr("cw_cleanup_confirm_body", undefined, "Deleted files can't be recovered.")}
        </p>
      </div>
    </Modal>
  );
}

function ResultCard({ summary, before, after }: { summary: ApplySummary; before: number | null; after: number | null }) {
  const tr = useTr();
  return (
    <section className="space-y-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3 text-sm" data-testid="cleanup-result">
      <div className="font-semibold">
        {tr(
          "cw_cleanup_result_removed",
          { count: summary.removed.length, size: formatBytes(summary.freed) },
          `Removed ${summary.removed.length} items (${formatBytes(summary.freed)})`,
        )}
      </div>
      {before !== null && after !== null ? (
        <div className="text-xs text-[var(--color-muted)]">
          {tr(
            "cw_cleanup_result_free",
            { before: formatBytes(before), after: formatBytes(after) },
            `Free on internal storage: ${formatBytes(before)} → ${formatBytes(after)}. It can take a minute to show.`,
          )}
        </div>
      ) : null}
      {[...summary.skipped.map((s) => ({ ...s, failed: false })), ...summary.failed.map((s) => ({ ...s, failed: true }))].map((r) => (
        <div key={r.item.id} className={`text-xs ${r.failed ? "text-[var(--color-bad)]" : "text-[var(--color-muted)]"}`}>
          {r.failed ? tr("cw_cleanup_result_failed", undefined, "Not removed:") : tr("cw_cleanup_result_skipped", undefined, "Skipped:")}{" "}
          {itemTitle(r.item)} — {reasonText(tr, r.reason, r.code)}
        </div>
      ))}
    </section>
  );
}

/**
 * Clean up: what the PS5 can give back (installed updates, leftover packages, ps5upload temp
 * files, downloads and leftovers of removed games), scanned and removed by the helper.
 */
export default function CleanupScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host)?.trim() ?? "";
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [scan, setScan] = useState<CleanupScan | null>(null);
  const [scanning, setScanning] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [confirming, setConfirming] = useState(false);
  // Kept in a store: it runs on the helper, whether or not this screen stays open.
  const { applying, summary, freeBefore, error: runError, stopping } = useCleanupRuns((st) => cleanupRunOf(st, host));
  const [error, setError] = useState<string | null>(null);
  const live = useRef(0);
  // "Plan space" from a refused install opens the planner with its size and drive filled in.
  const location = useLocation();
  const planRequest = useMemo(() => parseSpacePlanQuery(location.search), [location.search]);
  const [planning, setPlanning] = useState(planRequest !== null);
  useEffect(() => {
    if (planRequest) setPlanning(true);
  }, [planRequest]);

  const runScan = useCallback(
    async (keepSummary = false) => {
      if (!host) return;
      const run = ++live.current;
      setScanning(true);
      setError(null);
      if (!keepSummary && host) dismissCleanupResult(host);
      try {
        // A scan lives in the helper's memory: a helper restarted meanwhile (sent again, by this
        // or another device) has forgotten it. Start it again rather than stop with an error.
        for (let attempt = 0; ; attempt++) {
          const { scan_id } = await cleanupScanStart(host);
          let lost = false;
          for (;;) {
            let s: CleanupScan;
            try {
              s = await cleanupScanStatus(host, scan_id);
            } catch (e) {
              if (attempt < 2 && e instanceof CleanupError && (e.reason === "unknown_scan" || e.reason === "scan_expired")) {
                lost = true;
                break;
              }
              throw e;
            }
            if (run !== live.current) return;
            setScan(s);
            if (s.state !== "running") {
              setSelected(initialSelection(s.items ?? []));
              break;
            }
            await new Promise((r) => setTimeout(r, SCAN_POLL_MS));
          }
          if (!lost || run !== live.current) break;
        }
      } catch (e) {
        if (run === live.current) setError(errorText(tr, e));
      } finally {
        if (run === live.current) setScanning(false);
      }
    },
    [host, tr],
  );

  useEffect(() => {
    setScan(null);
    setSelected(new Set());
    if (host && payloadStatus === "up") {
      // A Remove still running (this window left and came back, or another started it) is shown;
      // a scan would only be refused until it ends. A result it left is kept for this visit.
      void attachRunningApply(host).then((running) => {
        if (!running) void runScan(true);
      });
    }
    // Leaving the screen (or a console switch) drops a scan still being polled, and a result
    // that has been seen.
    const gen = live;
    return () => {
      gen.current++;
      if (host) dismissCleanupResult(host);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, payloadStatus]);

  // A Remove ended while this is open: read the console again (after a helper restart, some items
  // may already be gone).
  const wasApplying = useRef(false);
  useEffect(() => {
    if (wasApplying.current && !applying) void runScan(true);
    wasApplying.current = !!applying;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [applying]);

  const items = useMemo(() => scan?.items ?? [], [scan]);
  const groups = useMemo(() => groupItems(items), [items]);
  const totals = useMemo(() => selectionTotals(items, selected), [items, selected]);
  const busy = scanning || applying !== null;

  async function stopScan() {
    if (scan && host) await cleanupCancel(host, scan.scan_id).catch(() => {});
  }

  async function remove() {
    if (!scan || !host) return;
    setConfirming(false);
    setError(null);
    const ids = items.filter((i) => selected.has(i.id) && isRemovable(i)).map((i) => i.id);
    try {
      const { apply_id } = await cleanupApply(host, scan.scan_id, ids);
      startedApply(host, await cleanupApplyStatus(host, apply_id));
      void followApply(host, apply_id, items, scan.free_bytes);
    } catch (e) {
      setError(errorText(tr, e));
    }
  }

  function stopApply() {
    if (host) stopCleanupApply(host);
  }

  function show(item: CleanupItem) {
    const dir = item.is_dir ? item.path : parentDir(item.path);
    saveFsLastPath(host, dir);
    navigate(`/files?path=${encodeURIComponent(dir)}`);
  }

  const doneCount = applying ? applying.done.length + applying.skipped.length + applying.failed.length : 0;

  return (
    <div className="app-page">
      <PageHeader
        icon={Eraser}
        title={tr("cw_cleanup_title", undefined, "Clean up")}
        description={tr(
          "cleanup_description",
          undefined,
          "Find what fills the PS5's drives and can safely go: updates, package files left behind, temporary files and leftovers of removed games. Nothing is removed until you pick it and confirm.",
        )}
        right={
          <div className="flex flex-wrap gap-2">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Wand2 size={12} />}
              onClick={() => setPlanning(true)}
              disabled={planning || !host || payloadStatus !== "up"}
              data-testid="cleanup-make-room"
            >
              {tr("cleanup_plan_open", undefined, "Make room")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={() => void runScan()}
              disabled={busy || !host || payloadStatus !== "up"}
            >
              {tr("cw_cleanup_scan_again", undefined, "Scan again")}
            </Button>
          </div>
        }
      />
      <ConnectionGate require="payload">
        <div className="min-w-0 space-y-3">
          {error || runError ? (
            <Callout tone="error" title={tr("cw_cleanup_failed", undefined, "Clean up stopped")}>
              {error ?? errorText(tr, runError)}
            </Callout>
          ) : null}
          {payloadStatus === "up" && !planning ? <NotPlayedCard host={host} /> : null}

          {planning && host ? (
            <SpacePlanner
              host={host}
              scanId={scan && scan.state === "done" && !scanning ? scan.scan_id : null}
              items={items}
              request={planRequest}
              onClose={() => setPlanning(false)}
              onRan={() => void runScan(true)}
            />
          ) : null}

          {scanning ? (
            <div className="flex flex-wrap items-center justify-center gap-2 text-xs text-[var(--color-muted)]" data-testid="cleanup-scanning">
              <Spinner size={12} />
              <span>{phaseText(tr, scan?.state === "running" ? scan.phase : "updates")}</span>
              {scan?.state === "running" && scan.found > 0 ? (
                <span className="tabular-nums">{tr("cw_cleanup_found_so_far", { count: scan.found }, `${scan.found} found so far`)}</span>
              ) : null}
              <Button variant="ghost" size="sm" leftIcon={<Square size={10} />} onClick={() => void stopScan()}>
                {tr("stop", undefined, "Stop")}
              </Button>
            </div>
          ) : null}

          {applying ? (
            <div className="flex flex-wrap items-center justify-center gap-2 text-xs" data-testid="cleanup-applying">
              <Spinner size={12} />
              <span className="tabular-nums">
                {stopping
                  ? tr("fs_bulk_stopping", undefined, "Stopping…")
                  : applying.phase === "waiting"
                  ? tr("cw_cleanup_waiting_sony", undefined, "Waiting for the PS5 to finish removing the update…")
                  : tr(
                      "cw_cleanup_removing",
                      { done: doneCount, total: applying.total, size: formatBytes(applying.freed_bytes) },
                      `Removing… ${doneCount} of ${applying.total}, ${formatBytes(applying.freed_bytes)} freed`,
                    )}
              </span>
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<Square size={10} />}
                onClick={stopApply}
                disabled={stopping}
                data-testid="cleanup-apply-stop"
              >
                {tr("stop", undefined, "Stop")}
              </Button>
            </div>
          ) : null}

          {summary ? <ResultCard summary={summary} before={freeBefore} after={scan && !scanning ? scan.free_bytes : null} /> : null}

          {scan && scan.state !== "running" ? (
            <>
              {!scan.appdb ? (
                <Callout tone="warn" title={tr("cw_cleanup_appdb_title", undefined, "The game list couldn't be read")}>
                  {tr(
                    "cw_cleanup_appdb_body",
                    undefined,
                    "Nothing that depends on a game being removed is listed, so nothing of an installed game can be offered by mistake.",
                  )}
                </Callout>
              ) : null}
              {!scan.can_remove_updates && items.some((i) => i.cat === "A") ? (
                <Callout tone="info" title={tr("cw_cleanup_no_patch_api_title", undefined, "Updates can't be removed here")}>
                  {tr("cw_cleanup_no_patch_api_body", undefined, "This firmware does not offer it to the app. Use Settings > Storage on the PS5.")}
                </Callout>
              ) : null}
              {scan.install_running ? (
                <Callout tone="info" title={tr("cw_cleanup_install_running_title", undefined, "The PS5 is installing something")}>
                  {tr("cw_cleanup_install_running_body", undefined, "You can look, but removing waits until the install is done.")}
                </Callout>
              ) : null}
              {scan.truncated ? (
                <p className="text-xs text-[var(--color-warn)]">
                  {tr("cw_cleanup_truncated", undefined, "There was more than one list can hold. Clean these up, then scan again for the rest.")}
                </p>
              ) : null}

              {groups.length === 0 ? (
                <EmptyState icon={Eraser} message={tr("cw_cleanup_nothing", undefined, "Nothing to clean up.")} />
              ) : (
                <>
                  <div className="sticky top-0 z-10 flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-2)] px-3 py-2">
                    <div className="min-w-0 flex-1 text-sm" data-testid="cleanup-selected">
                      {totals.count === 0
                        ? tr("cw_cleanup_selected_none", undefined, "Nothing selected")
                        : tr(
                            "cw_cleanup_selected",
                            { count: totals.count, where: selectionLine(tr, totals.byDrive) },
                            `Selected ${totals.count}: ${selectionLine(tr, totals.byDrive)}`,
                          )}
                    </div>
                    <Button
                      variant="danger"
                      size="sm"
                      leftIcon={<Trash2 size={14} />}
                      disabled={busy || totals.count === 0}
                      onClick={() => setConfirming(true)}
                      data-testid="cleanup-remove"
                    >
                      {tr("cw_cleanup_remove_size", { size: formatBytes(totals.bytes) }, `Remove ${formatBytes(totals.bytes)}`)}
                    </Button>
                  </div>
                  {groups.map((g) => (
                    <GroupCard
                      key={g.cat}
                      host={host}
                      group={g}
                      selected={selected}
                      disabled={busy}
                      onSelect={setSelected}
                      onShow={show}
                    />
                  ))}
                </>
              )}
              {scan.reserve_bytes > 0 ? (
                <p className="text-xs text-[var(--color-muted)]" data-testid="cleanup-reserve">
                  {tr(
                    "cw_cleanup_reserve",
                    { size: formatBytes(scan.reserve_bytes) },
                    `${formatBytes(scan.reserve_bytes)} of the internal drive is held by the file system. It comes back as files are deleted.`,
                  )}
                </p>
              ) : null}
            </>
          ) : null}
        </div>
      </ConnectionGate>
      {confirming && scan ? (
        <ConfirmRemove items={items} selected={selected} onCancel={() => setConfirming(false)} onConfirm={() => void remove()} />
      ) : null}
    </div>
  );
}
