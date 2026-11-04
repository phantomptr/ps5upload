import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowUp, ChevronRight, Files as FilesIcon, Folder, FolderOpen, RotateCw, Square } from "lucide-react";
import { formatBytes } from "../../../lib/format";
import { errorText, type Volume } from "../../home/Home";
import { focusElement } from "../../nav";
import { getLanguage, t } from "../../strings";
import { Modal } from "../../ui/Modal";
import { parentPath, volumeOf } from "../move";
import { barPercent, sortRows, startUsage, stopUsage, type UsageRow, type UsageSnapshot } from "./usage";
import "../search/search.css";

/** The status line under the path: progress while measuring, else how it ended. */
export function usageLine(s: UsageSnapshot | null, refused: boolean): { text: string; tone: "info" | "error" | "plain" } | null {
  if (refused) return { text: t("cw_files_usage_busy"), tone: "error" };
  if (!s) return { text: t("cw_home_loading"), tone: "plain" };
  const total = s.rows.reduce((a, r) => a + r.bytes, 0);
  const items = s.entries.toLocaleString(getLanguage());
  switch (s.end) {
    case null:
      return { text: t("cw_files_usage_running", { at: s.measured, n: s.folders, items }), tone: "plain" };
    case "done":
      return { text: t("cw_files_usage_done", { size: formatBytes(total), items }), tone: "plain" };
    case "capped":
      return { text: t("cw_files_usage_capped", { size: formatBytes(total), items }), tone: "info" };
    case "timeout":
      return { text: t("cw_files_usage_timeout", { size: formatBytes(total) }), tone: "info" };
    case "cancelled":
      return { text: t("cw_files_usage_cancelled", { size: formatBytes(total) }), tone: "info" };
    default: {
      const why = errorText(s.error);
      return why ? { text: why, tone: "error" } : null;
    }
  }
}

/** A row's size column: its size, with a mark when it is not the whole story. */
export function rowSize(r: UsageRow): string {
  if (r.state === "waiting") return "…";
  if (r.state === "skipped") return t("cw_files_usage_skipped");
  if (r.state === "unreadable") return t("cw_files_usage_unreadable");
  const s = formatBytes(r.bytes);
  return r.state === "partial" || r.state === "walking" ? `${s}+` : s;
}

/** The props-only view (tests render it). */
export function UsageView({
  root,
  canUp,
  snap,
  refused,
  onUp,
  onDrill,
  onOpen,
  onCancel,
  onAgain,
  onClose,
}: {
  root: string;
  canUp: boolean;
  snap: UsageSnapshot | null;
  refused: boolean;
  onUp: () => void;
  onDrill: (path: string) => void;
  onOpen: (path: string) => void;
  onCancel: () => void;
  onAgain: () => void;
  onClose: () => void;
}) {
  const rows = snap ? sortRows(snap.rows).filter((r) => !r.files || r.bytes > 0 || r.entries > 0) : [];
  const max = rows.reduce((a, r) => Math.max(a, r.bytes), 0);
  const line = usageLine(snap, refused);
  const running = snap != null && snap.end == null;
  return (
    <>
      <div className="cw-viewer-head">
        <h2 className="cw-file-name">{t("cw_files_usage")}</h2>
        <button type="button" className="cw-focusable cw-button" data-action="usage-close" onClick={onClose}>
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
      <div className="cw-tv-bar">
        <button type="button" className="cw-focusable cw-button" data-action="usage-up" aria-disabled={!canUp || undefined} onClick={onUp}>
          <ArrowUp aria-hidden />
          <span>{t("cw_files_up")}</span>
        </button>
        <span className="cw-mono cw-usage-path" data-usage-root={root}>
          {root}
        </span>
        <button type="button" className="cw-focusable cw-button" data-action="usage-open-root" onClick={() => onOpen(root)}>
          <FolderOpen aria-hidden />
          <span>{t("cw_files_usage_open")}</span>
        </button>
      </div>
      {line ? (
        <div className={`cw-msg${line.tone !== "plain" ? ` is-${line.tone}` : ""}`} role="status" data-usage-end={snap?.end ?? "running"}>
          {line.text}
          {running ? (
            <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="usage-cancel" onClick={onCancel}>
              <Square aria-hidden />
              <span>{t("cw_files_search_cancel")}</span>
            </button>
          ) : (
            <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="usage-again" onClick={onAgain}>
              <RotateCw aria-hidden />
              <span>{t("cw_files_usage_again")}</span>
            </button>
          )}
        </div>
      ) : null}
      <div className="cw-usage-body">
        {snap && snap.end != null && !rows.length ? <p className="cw-dim">{t("cw_files_empty")}</p> : null}
        <ul className="cw-files" data-nav-list aria-label={root}>
          {rows.map((r) => {
            const Icon = r.files ? FilesIcon : Folder;
            const pct = barPercent(r.bytes, max);
            return (
              <li key={r.path + (r.files ? "/" : "")} className="cw-file cw-usage-row" data-nav-item data-usage-row={r.files ? "" : r.name}>
                <button
                  type="button"
                  className="cw-focusable cw-file-main"
                  data-usage-open={r.path}
                  aria-label={t("cw_files_usage_open_row", { name: r.files ? t("cw_files_usage_files_here") : r.name })}
                  onClick={() => onOpen(r.path)}
                >
                  <Icon className="cw-file-icon" aria-hidden />
                  <span className="cw-file-text">
                    <span className="cw-file-name">{r.files ? t("cw_files_usage_files_here") : r.name}</span>
                  </span>
                  <span className="cw-usage-bar" aria-hidden>
                    <span className="cw-bar">
                      <span className={`cw-bar-fill${r.state === "walking" ? " is-unknown" : ""}`} style={{ display: "block", width: `${pct}%` }} />
                    </span>
                  </span>
                  <span className="cw-file-meta cw-dim cw-usage-size">{rowSize(r)}</span>
                </button>
                {!r.files ? (
                  <button
                    type="button"
                    className="cw-focusable cw-button"
                    data-usage-drill={r.path}
                    aria-label={t("cw_files_usage_inside", { name: r.name })}
                    data-tip={t("cw_files_usage_inside", { name: r.name })}
                    onClick={() => onDrill(r.path)}
                  >
                    <ChevronRight aria-hidden />
                  </button>
                ) : (
                  <span className="cw-usage-spacer" aria-hidden />
                )}
              </li>
            );
          })}
        </ul>
      </div>
    </>
  );
}

/**
 * Disk usage (a Files tool): the subfolders of a folder (at first the one
 * Files shows; Up goes as far as its drive) as bars, biggest first, measured one after another. "›" looks
 * inside a folder (a new measurement; ○ comes back up); ✕ on a row opens it in
 * Files. One measurement at a time; closing the dialog cancels it.
 */
export function UsageDialog({
  folder,
  volumes,
  onOpen,
  onClose,
}: {
  /** The folder Files shows: the measurement starts there (Up reaches its drive). */
  folder: string;
  volumes: Volume[];
  onOpen: (path: string) => void;
  onClose: () => void;
}) {
  // Up goes as far as the drive; the measurement starts at the folder Files shows.
  const top = (volumeOf(folder, volumes)?.path ?? folder).replace(/\/+$/, "") || "/";
  const [root, setRoot] = useState(() => folder.replace(/\/+$/, "") || "/");
  const [snap, setSnap] = useState<UsageSnapshot | null>(null);
  const [refused, setRefused] = useState(false);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let run: ReturnType<typeof startUsage> = null;
    let gone = false;
    setSnap(null);
    setRefused(false);
    // One at a time: the measurement shown before (a drill or Up) ends first.
    void stopUsage().then(() => {
      if (gone) return;
      run = startUsage(root, (s) => {
        if (!gone) setSnap(s);
      });
      if (!run) setRefused(true);
    });
    return () => {
      gone = true;
      run?.cancel();
    };
  }, [root, tick]);
  /** Where focus goes once it can: Up after looking inside, the folder we came out of after Up. */
  const focusNext = useRef<[string, string] | null>(null);
  useEffect(() => {
    const want = focusNext.current;
    if (!want || typeof document === "undefined") return;
    const [attr, value] = want;
    // Compared as-is (no CSS escaping of folder names).
    const el = Array.from(document.querySelectorAll<HTMLElement>(`[${attr}]`)).find((x) => x.getAttribute(attr) === value);
    if (el && el.getAttribute("aria-disabled") !== "true") {
      focusNext.current = null;
      focusElement(el);
    } else if (snap && snap.root === root && snap.end != null) focusNext.current = null; // it ended without that row
  }, [root, snap]);

  const canUp = root !== top && root !== "/";
  const onUp = useCallback(() => {
    if (root === top) return;
    focusNext.current = ["data-usage-drill", root];
    setRoot(parentPath(root));
  }, [root, top]);
  const onDrill = useCallback((path: string) => {
    focusNext.current = ["data-action", "usage-up"];
    setRoot(path);
  }, []);
  return (
    <Modal label={t("cw_files_usage")} className="cw-viewer cw-usage" onBack={canUp ? onUp : onClose}>
      <UsageView
        root={root}
        canUp={canUp}
        snap={snap}
        refused={refused}
        onUp={onUp}
        onDrill={onDrill}
        onOpen={onOpen}
        onCancel={() => void stopUsage()}
        onAgain={() => setTick((n) => n + 1)}
        onClose={onClose}
      />
    </Modal>
  );
}
