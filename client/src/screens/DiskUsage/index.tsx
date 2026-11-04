import { useEffect, useMemo, useRef, useState } from "react";
import { PieChart, RefreshCw, ChevronUp, Square, Eraser, FolderOpen } from "lucide-react";
import { Navigate, useNavigate } from "react-router";
import { useConnectionStore } from "../../state/connection";
import { Button, EmptyState, ErrorCard, Spinner } from "../../components";
import { humanizePs5Error, humanizeCaught } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";
import { mgmtAddr } from "../../lib/addr";
import {
  blocksOf,
  cachedFolder,
  forgetWalks,
  isWalkBusy,
  rememberWalk,
  runWalk,
  type DuBlock,
  type DuDir,
  type DuStatus,
} from "./duTree";
import { filesViewLink } from "../FileSystem/filesViews";

/** Disk usage lives in Files now ("What takes space?", `DiskUsagePanel`); the old route
 *  lands there. */
export default function DiskUsageScreen() {
  return <Navigate to={filesViewLink("space", { path: "/user" })} replace />;
}

/**
 * Disk usage treemap viewer.
 *
 * The engine walks the folder on the console (every folder below it, a few
 * lists at a time) and answers with each entry's recursive size, rendered as
 * a "squarified treemap" — each rectangle's area proportional to size. Click
 * a folder to drill in: the walk of its parent already holds that folder's
 * tree (kept per console for the session in `duTree`), so drilling in does
 * not walk again. Refresh walks again; leaving the view or the folder
 * cancels a walk that is still going.
 *
 * Shown in Files as "What takes space?", starting at `initialPath` (the folder or
 * drive it was opened from); `onOpenFolder` shows the folder it is on in Files.
 */
export function DiskUsagePanel({
  initialPath,
  onOpenFolder,
}: {
  initialPath: string;
  onOpenFolder?: (path: string) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [path, setPath] = useState(initialPath);
  const [dir, setDir] = useState<DuDir | null>(null);
  const [partial, setPartial] = useState<DuStatus["partial"]>(undefined);
  const [progress, setProgress] = useState<DuStatus | null>(null);
  const [scanning, setScanning] = useState(false);
  const [stopped, setStopped] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Bumped by Refresh: the walk effect runs again with the cache for this folder dropped.
  const [reload, setReload] = useState(0);
  const walkRef = useRef<AbortController | null>(null);

  // Reset to the starting folder and drop the previous console's tree the instant
  // the active console changes, so a switch never momentarily shows console
  // A's directories under console B's header. Not on mount: the walk below starts there.
  const shownHostRef = useRef(host);
  useEffect(() => {
    if (shownHostRef.current === host) return;
    shownHostRef.current = host;
    setPath(initialPath);
    setDir(null);
    setError(null);
    // Only a console switch resets; a new start folder remounts the panel (Files keys it).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  useEffect(() => {
    const h = host?.trim();
    if (!h || payloadStatus !== "up") return;
    setError(null);
    setStopped(false);
    const hit = cachedFolder(h, path);
    if (hit) {
      setDir(hit.dir);
      setPartial(hit.partial);
      setScanning(false);
      return;
    }
    // Each walk owns its abort: the cleanup (a new folder, a console switch, leaving the
    // screen) cancels it on the engine, and a late reply from it is dropped.
    const ac = new AbortController();
    walkRef.current = ac;
    setDir(null);
    setProgress(null);
    setScanning(true);
    runWalk(mgmtAddr(h), path, (s) => !ac.signal.aborted && setProgress(s), ac.signal)
      .then((s) => {
        if (!s || ac.signal.aborted) return;
        if (s.state === "failed") {
          setError(s.error ?? "failed");
          return;
        }
        rememberWalk(h, s);
        if (s.tree) {
          setDir(s.tree);
          setPartial(s.partial);
        }
      })
      .catch((e) => {
        if (!ac.signal.aborted) setError(humanizeCaught(e));
      })
      .finally(() => {
        if (!ac.signal.aborted) setScanning(false);
      });
    return () => ac.abort();
  }, [host, payloadStatus, path, reload]);

  function refresh() {
    const h = host?.trim();
    if (h) forgetWalks(h, path);
    setReload((n) => n + 1);
  }

  function stop() {
    walkRef.current?.abort();
    setScanning(false);
    setStopped(true);
  }

  const blocks = useMemo<DuBlock[] | null>(
    () =>
      dir
        ? blocksOf(dir).map((b) =>
            b.count !== undefined
              ? {
                  ...b,
                  name: tr("disk_usage_other_files", { count: b.count }, `${b.count} other files`),
                }
              : b,
          )
        : null,
    [dir, tr],
  );
  const totalBytes = dir?.bytes ?? 0;

  function up() {
    const trimmed = path.replace(/\/+$/, "");
    if (trimmed === "" || trimmed === "/") return;
    const parent = trimmed.slice(0, trimmed.lastIndexOf("/")) || "/";
    setPath(parent);
  }

  function drill(name: string, isDir: boolean) {
    if (!isDir) return;
    setPath((path === "/" ? "" : path) + "/" + name);
  }

  const PRESET_PATHS = ["/user", "/data", "/system_data", "/system_ex", "/mnt"];

  return (
    <div className="min-w-0 space-y-3">
      <div className="flex flex-wrap items-center justify-end gap-2">
        {onOpenFolder && (
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<FolderOpen size={12} />}
            onClick={() => onOpenFolder(path)}
            title={path}
          >
            {tr("fs_space_open_folder", undefined, "Open this folder")}
          </Button>
        )}
        <Button variant="ghost" size="sm" leftIcon={<Eraser size={12} />} onClick={() => navigate("/cleanup")}>
          {tr("cleanup_open", undefined, "Clean up…")}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<RefreshCw size={12} />}
          onClick={refresh}
          disabled={scanning || !host?.trim() || payloadStatus !== "up"}
        >
          {tr("refresh", undefined, "Refresh")}
        </Button>
      </div>
      {/* Breadcrumb row: Up button + truncated path + total. The path
       * uses min-w-0 + truncate so a deep path can't push the right
       * edge past the viewport. Presets moved to their own row below
       * so they always fit regardless of path length. */}
      <div className="flex items-center gap-2 text-xs">
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<ChevronUp size={11} />}
          onClick={up}
          disabled={path === "/" || path === ""}
        >
          {tr("disk_usage_up", undefined, "Up")}
        </Button>
        <span
          className="min-w-0 flex-1 truncate font-mono text-[var(--color-muted)]"
          title={path}
        >
          {path}
        </span>
        <span className="shrink-0 tabular-nums">
          {formatBytes(totalBytes)}
        </span>
      </div>
      <div className="flex flex-wrap gap-1">
        {PRESET_PATHS.map((p) => (
          <button
            key={p}
            type="button"
            onClick={() => setPath(p)}
            aria-pressed={path === p}
            className="chip px-3 py-1 font-mono text-xs"
          >
            {p}
          </button>
        ))}
      </div>

      {error && (
        <ErrorCard
          title={tr("disk_usage_failed", undefined, "Couldn't read this folder")}
          detail={
            isWalkBusy(error)
              ? tr(
                  "disk_usage_busy",
                  undefined,
                  "Another folder on this PS5 is being counted. Wait for it to finish, then try again.",
                )
              : humanizePs5Error(error) || error
          }
          action={
            <Button variant="secondary" size="sm" onClick={refresh}>
              {tr("disk_usage_retry", undefined, "Retry")}
            </Button>
          }
        />
      )}

      {scanning && (
        <div
          className="flex flex-wrap items-center justify-center gap-2 text-xs text-[var(--color-muted)]"
          data-testid="disk-usage-progress"
        >
          <Spinner size={12} />
          <span className="tabular-nums">
            {tr(
              "disk_usage_scanning",
              {
                dirs: progress?.dirs_scanned ?? 0,
                size: formatBytes(progress?.bytes ?? 0),
              },
              `Counting… ${progress?.dirs_scanned ?? 0} folders, ${formatBytes(progress?.bytes ?? 0)} so far`,
            )}
          </span>
          <Button variant="ghost" size="sm" leftIcon={<Square size={10} />} onClick={stop}>
            {tr("stop", undefined, "Stop")}
          </Button>
        </div>
      )}

      {stopped && !scanning && !dir && (
        <EmptyState
          icon={PieChart}
          message={tr("disk_usage_stopped", undefined, "Counting stopped.")}
          action={
            <Button variant="secondary" size="sm" onClick={refresh}>
              {tr("disk_usage_retry", undefined, "Retry")}
            </Button>
          }
        />
      )}

      {dir && <DiskUsageNotes dir={dir} partial={partial} />}

      {blocks && blocks.length === 0 && !scanning && (
        <EmptyState
          icon={PieChart}
          message={tr("disk_usage_empty", undefined, "Folder is empty.")}
        />
      )}

      {blocks && blocks.length > 0 && totalBytes > 0 && (
        <Treemap nodes={blocks} totalBytes={totalBytes} onDrill={drill} />
      )}

      {/* Nothing here takes any space (only empty folders or empty files), so the
          treemap has no area to share out. A drillable list instead of a blank panel. */}
      {blocks && blocks.length > 0 && totalBytes === 0 && !scanning && (
        <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-card)] border border-[var(--glass-edge)]">
          {blocks.map((n) => (
            <li key={n.name}>
              <button
                type="button"
                onClick={() => drill(n.name, n.isDir)}
                disabled={!n.isDir}
                className="flex w-full items-center justify-between px-3 py-2 text-start text-sm hover:bg-[var(--color-surface-3)] disabled:cursor-default disabled:hover:bg-transparent"
              >
                <span className="truncate">
                  {n.isDir ? "📁 " : "📄 "}
                  {n.name}
                </span>
                <span className="ms-3 shrink-0 text-xs text-[var(--color-muted)]">
                  {formatBytes(n.totalSize)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** What the walk counted, and what it could not. */
export function DiskUsageNotes({
  dir,
  partial,
}: {
  dir: DuDir;
  partial?: DuStatus["partial"];
}) {
  const tr = useTr();
  return (
    <div className="space-y-1 text-xs text-[var(--color-muted)]">
      <p className="tabular-nums">
        {tr(
          "disk_usage_summary",
          { files: dir.files, dirs: dir.dirs },
          `${dir.files} files in ${dir.dirs} folders`,
        )}
      </p>
      {partial && (
        <p className="text-[var(--color-warn)]">
          {tr(
            "disk_usage_partial",
            undefined,
            "Counting stopped before it reached every folder, so these sizes are lower than the real ones.",
          )}
        </p>
      )}
      {dir.skipped > 0 && (
        <p>
          {tr(
            "disk_usage_skipped",
            { count: dir.skipped },
            `${dir.skipped} folders were not counted: they could not be read, belong to the system, or are on another drive.`,
          )}
        </p>
      )}
    </div>
  );
}

/** Squarified-ish layout — sort by size desc and pack into rows.
 *  Not perfectly squarified (which is its own algorithm), but close
 *  enough to be readable for top-N folder sums. */
function Treemap({
  nodes,
  totalBytes,
  onDrill,
}: {
  nodes: DuBlock[];
  totalBytes: number;
  onDrill: (name: string, isDir: boolean) => void;
}) {
  const tr = useTr();
  // Cap at top 30 to keep visual readable; aggregate the rest as
  // an "other" rectangle so the total still adds up.
  const TOP_N = 30;
  const top = nodes.slice(0, TOP_N);
  const rest = nodes.slice(TOP_N);
  const restTotal = rest.reduce((acc, n) => acc + n.totalSize, 0);
  const display =
    restTotal > 0
      ? [
          ...top,
          {
            name: tr("disk_usage_more", { count: rest.length }, `(+${rest.length} more)`),
            isDir: false,
            totalSize: restTotal,
          },
        ]
      : top;

  // Simple row-packing: walk through items in size-desc order, each
  // row gets entries until we hit a heuristic threshold.
  const rows: DuBlock[][] = [];
  let currentRow: DuBlock[] = [];
  let currentRowSize = 0;
  const targetRowSize = totalBytes / Math.ceil(Math.sqrt(display.length));
  for (const n of display) {
    currentRow.push(n);
    currentRowSize += n.totalSize;
    if (currentRowSize >= targetRowSize && currentRow.length > 0) {
      rows.push(currentRow);
      currentRow = [];
      currentRowSize = 0;
    }
  }
  if (currentRow.length > 0) rows.push(currentRow);

  return (
    <div
      className="grid gap-1.5 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-2 shadow-[var(--edge-highlight),var(--shadow-1)]"
      style={{ gridAutoFlow: "row" }}
    >
      {rows.map((row, ri) => {
        const rowTotal = row.reduce((acc, n) => acc + n.totalSize, 0);
        const rowFraction = rowTotal / totalBytes;
        return (
          <div
            key={ri}
            className="flex flex-wrap gap-1.5"
            style={{
              /* Row height = its share of the total bytes, mapped onto
               * a 320 px treemap canvas. At least 40 px, so both the
               * name and the size line fit even when a row carries a
               * tiny fraction of the total.
               *
               * flex-wrap lets cells flow to a new line if their
               * combined minWidth (96 px each) exceeds the container —
               * matters most on narrow windows or rows with many tiny
               * folders. Without wrap, cells would clip past the right
               * edge once min-widths summed past 100 %.
               *
               * This is a MINIMUM height, never a fixed one: with a fixed
               * height a row that wrapped kept the height of one line, and
               * its second line of cells was drawn below the panel's
               * bottom border. */
              minHeight: `${Math.max(40, rowFraction * 320)}px`,
            }}
          >
            {row.map((n, i) => {
              // Bigger blocks are more saturated; the hue cycles through the palette so
              // neighbours stay apart. Tints of the theme tokens, so both modes follow.
              const strength = Math.round(
                Math.min(62, 24 + (n.totalSize / totalBytes) * 160),
              );
              const hue = TREEMAP_HUES[(ri * 3 + i) % TREEMAP_HUES.length];
              return (
                <button
                  key={n.name}
                  type="button"
                  onClick={() => onDrill(n.name, n.isDir)}
                  disabled={!n.isDir}
                  title={`${n.name} — ${formatBytes(n.totalSize)}`}
                  className={`flex min-h-10 min-w-0 flex-col justify-center overflow-hidden rounded-[var(--radius-field)] border border-[var(--glass-edge)] px-2.5 py-1 text-start text-xs text-[var(--color-text)] transition ${
                    n.isDir
                      ? "cursor-pointer hover:opacity-80"
                      : "cursor-default"
                  }`}
                  /* flex-grow with size as weight: row width is shared
                   * proportional to size while gaps come out of the
                   * grow budget — no overflow even with gap-1 between
                   * cells. Avoids the classic "% widths sum to 100 +
                   * (n-1)*gap → last cell pushed past the right edge"
                   * bug that made the treemap clip on wide rows.
                   *
                   * minWidth (96 px) trades a small amount of strict
                   * proportionality for text readability: without it,
                   * many tiny folders in one row each got a ~6 px slice
                   * and both name + size were truncated to a single
                   * character — visually a colored bar with no label.
                   * 96 px fits a size like "57.3 GiB" whole (at 64 px it
                   * read "57.…") and the start of a name. Cells
                   * larger than minWidth still distribute the extra
                   * space proportionally via flex-grow, so the rough
                   * "bigger folder = bigger area" still holds. */
                  style={{
                    flexGrow: n.totalSize,
                    flexBasis: 0,
                    minWidth: "96px",
                    backgroundColor: `color-mix(in srgb, var(${hue}) ${n.isDir ? strength : Math.round(strength * 0.6)}%, var(--color-surface-raised))`,
                  }}
                >
                  <div className="truncate text-xs font-medium leading-tight">
                    {n.name}
                  </div>
                  <div className="truncate font-mono text-xs leading-tight opacity-85">
                    {formatBytes(n.totalSize)}
                  </div>
                </button>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}

/** Treemap block colours: the theme's own accent and status tones. */
const TREEMAP_HUES = [
  "--color-accent-bright",
  "--color-ps5",
  "--color-accent-pink",
  "--color-ps4",
  "--color-warn",
  "--color-good",
];

// formatBytes moved to lib/format.ts.
