import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Check, FolderOpen, RefreshCw, Square, Trash2 } from "lucide-react";
import type { CleanupApplyStatus, CleanupItem, CleanupScan } from "../../api/cleanup";
import {
  groupItems,
  initialSelection,
  isRemovable,
  itemBytes,
  parentDir,
  selectionTotals,
  summarizeApply,
  toggle,
  type ApplySummary,
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
} from "../../lib/cleanupText";
import { formatBytes } from "../../lib/format";
import { ApiError, call, isOffline } from "../api";
import { onNavAction } from "../nav";
import type { SectionContext } from "../sections";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Modal } from "../ui/Modal";

const SCAN_POLL_MS = 800;
const APPLY_POLL_MS = 1200;

/** What the last finished scan of this page found (Home's card shows it). */
let lastFound: { bytes: number; count: number } | null = null;
const foundListeners = new Set<() => void>();

export function cleanupLastFound(): { bytes: number; count: number } | null {
  return lastFound;
}

/** Called whenever a scan of this page finishes. Returns the unsubscribe function. */
export function onCleanupFound(fn: () => void): () => void {
  foundListeners.add(fn);
  return () => {
    foundListeners.delete(fn);
  };
}

function noteFound(items: CleanupItem[]) {
  const rem = items.filter(isRemovable);
  lastFound = { bytes: rem.reduce((n, i) => n + itemBytes(i), 0), count: rem.length };
  for (const fn of Array.from(foundListeners)) fn();
}

/** A failed call as one translated line. */
export function cleanupErrorText(err: unknown): string {
  if (err instanceof ApiError) {
    if (isOffline(err)) return t("cw_home_err_offline");
    return refusalText(t, err.code) ?? t("cw_home_err_failed", { code: err.code });
  }
  return t("cw_home_err_failed", { code: err instanceof Error ? err.message : String(err) });
}

function itemTitle(item: CleanupItem): string {
  if (item.name) return item.name;
  if (item.cat === "C" || item.cat === "D" || item.cat === "G" || !item.title_id) {
    const i = item.path.lastIndexOf("/");
    return i >= 0 ? item.path.slice(i + 1) : item.path;
  }
  return item.title_id;
}

export interface CleanupViewProps {
  /** The System section's view switch, shown under the title. */
  switcher?: ReactNode;
  scan: CleanupScan | null;
  scanning: boolean;
  /** Scanning, but still waiting for a scan or removal already running in the helper to end. */
  waiting?: boolean;
  applying: CleanupApplyStatus | null;
  summary: ApplySummary | null;
  selected: Set<number>;
  message: string | null;
  onToggle: (id: number) => void;
  onRescan: () => void;
  onStop: () => void;
  onRemove: () => void;
  /** Open Files at a folder ("worth a look" items), or null when Files cannot open a path. */
  onShow: ((path: string) => void) | null;
}

/** One item: a check button (✕ ticks it), or for "worth a look" a button that opens it in Files. */
function Row({
  item,
  checked,
  disabled,
  onToggle,
  onShow,
}: {
  item: CleanupItem;
  checked: boolean;
  disabled: boolean;
  onToggle: (id: number) => void;
  onShow: ((path: string) => void) | null;
}) {
  const removable = isRemovable(item);
  const body = (
    <>
      <span className="cw-cleanup-check" aria-hidden>
        {removable ? checked ? <Check /> : null : <FolderOpen />}
      </span>
      <span className="cw-cleanup-text">
        <span className="cw-cleanup-name">
          {itemTitle(item)}
          <span className="cw-tag">{driveName(t, item.drive)}</span>
        </span>
        <span className="cw-dim cw-cleanup-desc">{itemDetails(t, item)}</span>
      </span>
      <span className="cw-cleanup-size">{formatBytes(itemBytes(item))}</span>
    </>
  );
  if (!removable) {
    const dir = item.is_dir ? item.path : parentDir(item.path);
    return (
      <li className="cw-cleanup-item">
        <button
          type="button"
          className="cw-focusable cw-button cw-cleanup-row"
          data-cleanup-id={item.id}
          aria-disabled={!onShow || undefined}
          onClick={() => onShow?.(dir)}
        >
          {body}
        </button>
      </li>
    );
  }
  return (
    <li className="cw-cleanup-item">
      <button
        type="button"
        role="checkbox"
        aria-checked={checked}
        aria-disabled={disabled || undefined}
        className={`cw-focusable cw-button cw-cleanup-row${checked ? " is-on" : ""}`}
        data-cleanup-id={item.id}
        onClick={() => {
          if (!disabled) onToggle(item.id);
        }}
      >
        {body}
      </button>
    </li>
  );
}

/** Clean up without state: everything comes in through props (tests render it directly). */
export function CleanupView(p: CleanupViewProps) {
  const items = useMemo(() => p.scan?.items ?? [], [p.scan]);
  const groups = useMemo(() => groupItems(items), [items]);
  const totals = selectionTotals(items, p.selected);
  const busy = p.scanning || p.applying !== null;
  const done = p.applying ? p.applying.done.length + p.applying.skipped.length + p.applying.failed.length : 0;
  const finished = p.scan && p.scan.state !== "running";

  return (
    <div className="cw-cleanup">
      <h1 className="cw-h1">{t("cw_cleanup_title")}</h1>
      {p.switcher}
      {p.message ? (
        <div className="cw-msg is-error" role="alert">
          {p.message}
        </div>
      ) : null}
      {p.scanning ? (
        <div className="cw-card cw-cleanup-progress" role="status">
          <span>{p.waiting ? t("cw_cleanup_waiting_other") : phaseText(t, p.scan?.state === "running" ? p.scan.phase : "updates")}</span>
          {!p.waiting && p.scan?.state === "running" && p.scan.found > 0 ? (
            <span className="cw-dim"> {t("cw_cleanup_found_so_far", { count: p.scan.found })}</span>
          ) : null}
          <button type="button" className="cw-focusable cw-button" data-action="cleanup-stop" onClick={p.onStop}>
            <Square aria-hidden />
            <span>{t("cw_files_batch_stop")}</span>
          </button>
        </div>
      ) : null}
      {p.applying ? (
        <div className="cw-card cw-cleanup-progress" role="status">
          <span>
            {p.applying.phase === "waiting"
              ? t("cw_cleanup_waiting_sony")
              : t("cw_cleanup_removing", {
                  done,
                  total: p.applying.total,
                  size: formatBytes(p.applying.freed_bytes),
                })}
          </span>
          <button type="button" className="cw-focusable cw-button" data-action="cleanup-stop" onClick={p.onStop}>
            <Square aria-hidden />
            <span>{t("cw_files_batch_stop")}</span>
          </button>
        </div>
      ) : null}
      {p.summary ? (
        <div className="cw-card cw-cleanup-result" role="status">
          <div className="cw-cleanup-name">
            {t("cw_cleanup_result_removed_v2", { count: p.summary.removed.length, size: formatBytes(p.summary.freed) })}
          </div>
          {p.summary.skipped.map((s) => (
            <div key={`s${s.item.id}`} className="cw-dim">
              {t("cw_cleanup_result_skipped")} {itemTitle(s.item)} — {reasonText(t, s.reason, s.code)}
            </div>
          ))}
          {p.summary.failed.map((s) => (
            <div key={`f${s.item.id}`} className="cw-cleanup-failed">
              {t("cw_cleanup_result_failed")} {itemTitle(s.item)} — {reasonText(t, s.reason, s.code)}
            </div>
          ))}
        </div>
      ) : null}
      {finished && p.scan && !p.scan.appdb ? <p className="cw-dim">{t("cw_cleanup_appdb_body")}</p> : null}
      {finished && p.scan && p.scan.install_running ? <p className="cw-dim">{t("cw_cleanup_install_running_body")}</p> : null}
      {finished && groups.length === 0 ? <p className="cw-dim">{t("cw_cleanup_nothing")}</p> : null}
      {finished && items.some(isRemovable) ? <p className="cw-dim cw-cleanup-how">{t("cw_cleanup_how")}</p> : null}
      {groups.map((g) => (
        <section key={g.cat} className="cw-cleanup-group" aria-label={categoryTitle(t, g.cat)}>
          <h2 className="cw-h2">
            {categoryTitle(t, g.cat)} <span className="cw-dim">{formatBytes(g.bytes)}</span>
          </h2>
          <p className="cw-dim cw-cleanup-hint">{categoryHint(t, g.cat)}</p>
          <ul className="cw-cleanup-list">
            {g.items.map((i) => (
              <Row
                key={i.id}
                item={i}
                checked={p.selected.has(i.id)}
                disabled={busy}
                onToggle={p.onToggle}
                onShow={p.onShow}
              />
            ))}
          </ul>
        </section>
      ))}
      {finished && p.scan && p.scan.reserve_bytes > 0 ? (
        <p className="cw-dim cw-cleanup-hint">{t("cw_cleanup_reserve", { size: formatBytes(p.scan.reserve_bytes) })}</p>
      ) : null}
      <div className="cw-cleanup-bar">
        <span className="cw-cleanup-bar-text">
          {totals.count === 0
            ? t("cw_cleanup_selected_none")
            : t("cw_cleanup_selected", { count: totals.count, where: selectionLine(t, totals.byDrive) })}
        </span>
        <button
          type="button"
          className="cw-focusable cw-button"
          data-action="cleanup-rescan"
          aria-disabled={busy || undefined}
          onClick={() => {
            if (!busy) p.onRescan();
          }}
        >
          <RefreshCw aria-hidden />
          <span>{t("cw_cleanup_scan_again")}</span>
        </button>
        <button
          type="button"
          className="cw-focusable cw-button is-danger"
          data-action="cleanup-remove"
          aria-disabled={busy || totals.count === 0 || undefined}
          onClick={() => {
            if (!busy && totals.count > 0) p.onRemove();
          }}
        >
          <Trash2 aria-hidden />
          <span>{t("cw_cleanup_remove_size", { size: formatBytes(totals.bytes) })}</span>
        </button>
      </div>
    </div>
  );
}

/** The confirm text: what goes, the downgrade warning when updates are picked, and "can't be undone". */
export function confirmBody(items: CleanupItem[], selected: Set<number>): string {
  const totals = selectionTotals(items, selected);
  const parts = [t("cw_cleanup_selected", { count: totals.count, where: selectionLine(t, totals.byDrive) })];
  if (totals.downgrades.length) parts.push(`${t("cw_cleanup_downgrade_title")}: ${t("cw_cleanup_downgrade_body")}`);
  parts.push(t("cw_cleanup_confirm_body"));
  return parts.join("\n\n");
}

/** △ on a focused item: its id inside `root`, or null. */
export function focusedItemId(root: Element | null, active: Element | null): number | null {
  if (!root || !active || !root.contains(active)) return null;
  const el = active.closest("[data-cleanup-id]");
  if (!el) return null;
  const n = Number(el.getAttribute("data-cleanup-id"));
  return Number.isInteger(n) ? n : null;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The Clean up section: scan the console, tick what goes (✕), △ for details, then Remove. */
export function Cleanup({ ctx, switcher }: { ctx: SectionContext; switcher?: ReactNode }) {
  const [scan, setScan] = useState<CleanupScan | null>(null);
  const [scanning, setScanning] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [applying, setApplying] = useState<CleanupApplyStatus | null>(null);
  const [summary, setSummary] = useState<ApplySummary | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const [details, setDetails] = useState<CleanupItem | null>(null);
  const live = useRef(0);
  const mounted = useRef(true);
  const root = useRef<HTMLDivElement>(null);
  const scanRef = useRef<CleanupScan | null>(null);
  scanRef.current = scan;
  /** The scan this page started and is still polling (stopped when the page is left). */
  const polling = useRef<number | null>(null);

  const runScan = useCallback(async (keepSummary = false) => {
    const run = ++live.current;
    setScanning(true);
    setMessage(null);
    if (!keepSummary) setSummary(null);
    try {
      let scan_id: number;
      for (;;) {
        try {
          ({ scan_id } = await call<{ scan_id: number }>("/api/cleanup/scan"));
          break;
        } catch (e) {
          // The helper runs one scan or removal at a time: one is still going (this page was
          // left during it, or the PC app's). Wait for its end, then scan.
          if (!(e instanceof ApiError && e.code === "busy")) throw e;
          if (run !== live.current) return;
          setWaiting(true);
          await sleep(SCAN_POLL_MS);
        }
      }
      if (run !== live.current) {
        void call("/api/cleanup/cancel", { id: scan_id }).catch(() => {});
        return;
      }
      setWaiting(false);
      polling.current = scan_id;
      for (;;) {
        const s = await call<CleanupScan>("/api/cleanup/status", { scan_id });
        if (run !== live.current) return;
        setScan(s);
        if (s.state !== "running") {
          setSelected(initialSelection(s.items ?? []));
          noteFound(s.items ?? []);
          break;
        }
        await sleep(SCAN_POLL_MS);
      }
    } catch (e) {
      if (run === live.current) setMessage(cleanupErrorText(e));
    } finally {
      if (run === live.current) {
        polling.current = null;
        setScanning(false);
        setWaiting(false);
      }
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void runScan();
    // Leaving the screen (or a console switch) drops a scan still being polled, and stops it in
    // the helper (it is this page's alone; coming back would otherwise wait on it). A removal
    // goes on: it was confirmed.
    const gen = live;
    const own = polling;
    const here = mounted;
    return () => {
      here.current = false;
      gen.current++;
      const id = own.current;
      own.current = null;
      if (id) void call("/api/cleanup/cancel", { id }).catch(() => {});
    };
  }, [runScan]);

  const stop = useCallback(() => {
    if (waiting) {
      // Stop the wait for the other run (not that run: it is not this page's).
      live.current++;
      setScanning(false);
      setWaiting(false);
      return;
    }
    // the scan being polled: the last status read may still be the previous scan's
    const id = applying?.apply_id ?? (scanning ? (polling.current ?? undefined) : undefined);
    if (id) void call("/api/cleanup/cancel", { id }).catch(() => {});
  }, [applying, scanning, waiting]);

  const remove = useCallback(async () => {
    const s = scanRef.current;
    setAsking(false);
    if (!s) return;
    const items = s.items ?? [];
    const ids = items.filter((i) => selected.has(i.id) && isRemovable(i)).map((i) => i.id);
    setMessage(null);
    try {
      const { apply_id } = await call<{ apply_id: number }>("/api/cleanup/apply", { scan_id: s.scan_id, item_ids: ids });
      let st = await call<CleanupApplyStatus>("/api/cleanup/apply_status", { apply_id });
      setApplying(st);
      while (st.state === "running") {
        await sleep(APPLY_POLL_MS);
        st = await call<CleanupApplyStatus>("/api/cleanup/apply_status", { apply_id });
        setApplying(st);
      }
      setSummary(summarizeApply(st, items));
    } catch (e) {
      setMessage(cleanupErrorText(e));
    } finally {
      setApplying(null);
    }
    // Not after the page was left: that scan would be nobody's, and block the next opening.
    if (mounted.current) void runScan(true);
  }, [runScan, selected]);

  // Create scans again; △ shows the focused item's details.
  useEffect(
    () =>
      onNavAction(
        (a) => {
          if (a === "refresh") {
            if (!scanning && !applying) void runScan();
            return true;
          }
          if (a === "actions") {
            const id = focusedItemId(root.current, document.activeElement);
            const item = id != null ? scanRef.current?.items?.find((i) => i.id === id) : undefined;
            if (!item) return false;
            setDetails(item);
            return true;
          }
          return false;
        },
        () => root.current,
      ),
    [runScan, scanning, applying],
  );

  const onShow = ctx.canOpen("files") ? (path: string) => ctx.openSection("files", { path }) : null;
  const items = scan?.items ?? [];
  const totals = selectionTotals(items, selected);

  return (
    <div ref={root}>
      <CleanupView
        switcher={switcher}
        scan={scan}
        scanning={scanning}
        waiting={waiting}
        applying={applying}
        summary={summary}
        selected={selected}
        message={message}
        onToggle={(id) => setSelected((s) => toggle(s, id))}
        onRescan={() => void runScan()}
        onStop={stop}
        onRemove={() => setAsking(true)}
        onShow={onShow}
      />
      {asking ? (
        <ConfirmDialog
          title={t("cw_cleanup_confirm_title_v2", { count: totals.count })}
          body={confirmBody(items, selected)}
          confirmLabel={t("cw_cleanup_remove_size", { size: formatBytes(totals.bytes) })}
          danger
          onConfirm={() => void remove()}
          onCancel={() => setAsking(false)}
        />
      ) : null}
      {details ? (
        <Modal label={t("cw_cleanup_details_title")} onBack={() => setDetails(null)}>
          <h2>{itemTitle(details)}</h2>
          <p className="cw-dim">{categoryTitle(t, details.cat)}</p>
          <p>{itemDetails(t, details)}</p>
          <p className="cw-mono cw-cleanup-path">{details.path}</p>
          <p>{formatBytes(itemBytes(details))}</p>
          <div className="cw-dialog-buttons">
            <button type="button" className="cw-focusable cw-button" data-nav-default onClick={() => setDetails(null)}>
              <span>{t("cw_files_close")}</span>
            </button>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}
