import { useCallback, useEffect, useMemo, useState } from "react";
import {
  PieChart,
  RefreshCw,
  Loader2,
  ChevronUp,
  AlertTriangle,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

interface DirListResult {
  entries?: Array<{ name?: string; kind?: string; size?: number }>;
}

async function listDirRaw(
  addr: string,
  path: string,
  limit: number,
): Promise<DirListResult> {
  return invoke<DirListResult>("ps5_list_dir", {
    addr,
    path,
    offset: 0,
    limit,
  });
}
import { useConnectionStore } from "../../state/connection";
import { PageHeader, Button, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";

interface DirNode {
  name: string;
  size: number;
  isDir: boolean;
  /** Cumulative recursive size — populated after walk. */
  totalSize: number;
}

/**
 * Disk usage treemap viewer.
 *
 * Walks a starting path with FS_LIST_DIR (one level per dir),
 * accumulates cumulative sizes, and renders the result as a
 * "squarified treemap" — each rectangle's area proportional to
 * size. Click a directory to drill in.
 *
 * Walk depth is bounded to 3 levels by default to keep latency
 * manageable on slow LANs; the user can drill into deeper levels
 * by clicking. We don't mirror the FileSystem screen's full
 * navigation — this is purely visualization.
 */
export default function DiskUsageScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [path, setPath] = useState("/user");
  const [nodes, setNodes] = useState<DirNode[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      const listing = await listDirRaw(`${host.trim()}:9114`, path, 2048);
      // First pass: top-level entries with their direct sizes.
      // For directories we recurse one more level so the pie has
      // meaningful sizes for each top-level folder.
      const result: DirNode[] = [];
      const entries = listing.entries ?? [];
      for (const e of entries) {
        if (!e.name) continue;
        if (e.kind === "dir") {
          // Recursive walk capped at depth 1 (so /user/app sums all
          // its immediate children but doesn't fan out further).
          let total = 0;
          try {
            const childPath = path === "/" ? `/${e.name}` : `${path}/${e.name}`;
            const child = await listDirRaw(`${host.trim()}:9114`, childPath, 2048);
            for (const c of child.entries ?? []) {
              total += c.size ?? 0;
            }
          } catch {
            // Permission denied or empty — treat as zero.
            total = 0;
          }
          result.push({
            name: e.name,
            size: e.size ?? 0,
            isDir: true,
            totalSize: total,
          });
        } else {
          result.push({
            name: e.name,
            size: e.size ?? 0,
            isDir: false,
            totalSize: e.size ?? 0,
          });
        }
      }
      result.sort((a, b) => b.totalSize - a.totalSize);
      setNodes(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [host, payloadStatus, path]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const totalBytes = useMemo(() => {
    return (nodes ?? []).reduce((acc, n) => acc + n.totalSize, 0);
  }, [nodes]);

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
    <div className="p-6">
      <PageHeader
        icon={PieChart}
        title={tr("disk_usage_title", undefined, "Disk usage")}
        description={tr(
          "disk_usage_description",
          undefined,
          "Treemap of folder sizes on the PS5. Walks one level deep to compute sums; click a directory to drill in.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={refresh}
            disabled={loading || !host?.trim() || payloadStatus !== "up"}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />
      {payloadStatus !== "up" ? (
        <EmptyState
          icon={PieChart}
          message={tr(
            "disk_usage_no_payload",
            undefined,
            "Connect to your PS5 first.",
          )}
        />
      ) : (
        <div className="min-w-0 space-y-3">
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
            <span className="shrink-0 tabular-nums">{formatBytes(totalBytes)}</span>
          </div>
          <div className="flex flex-wrap gap-1">
            {PRESET_PATHS.map((p) => (
              <button
                key={p}
                type="button"
                onClick={() => setPath(p)}
                className={`rounded px-2 py-0.5 text-[10px] ${
                  path === p
                    ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                    : "border border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
                }`}
              >
                {p}
              </button>
            ))}
          </div>

          {error && (
            <div className="flex items-start gap-1 rounded-md border border-[var(--color-bad)] p-2 text-[11px] text-[var(--color-bad)]">
              <AlertTriangle size={11} className="mt-0.5 shrink-0" />
              {error}
            </div>
          )}

          {loading && nodes === null && (
            <div className="text-center text-xs text-[var(--color-muted)]">
              <Loader2 size={12} className="mr-2 inline animate-spin" />
              {tr("disk_usage_walking", undefined, "Walking…")}
            </div>
          )}

          {nodes && nodes.length === 0 && !loading && (
            <EmptyState
              icon={PieChart}
              message={tr("disk_usage_empty", undefined, "Folder is empty.")}
            />
          )}

          {nodes && nodes.length > 0 && totalBytes > 0 && (
            <Treemap nodes={nodes} totalBytes={totalBytes} onDrill={drill} />
          )}
        </div>
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
  nodes: DirNode[];
  totalBytes: number;
  onDrill: (name: string, isDir: boolean) => void;
}) {
  // Cap at top 30 to keep visual readable; aggregate the rest as
  // an "other" rectangle so the total still adds up.
  const TOP_N = 30;
  const top = nodes.slice(0, TOP_N);
  const rest = nodes.slice(TOP_N);
  const restTotal = rest.reduce((acc, n) => acc + n.totalSize, 0);
  const display = restTotal > 0
    ? [...top, { name: `(+${rest.length} more)`, size: 0, isDir: false, totalSize: restTotal }]
    : top;

  // Simple row-packing: walk through items in size-desc order, each
  // row gets entries until we hit a heuristic threshold.
  const rows: DirNode[][] = [];
  let currentRow: DirNode[] = [];
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
      className="grid gap-1 rounded-md border border-[var(--color-border)] p-1"
      style={{ gridAutoFlow: "row" }}
    >
      {rows.map((row, ri) => {
        const rowTotal = row.reduce((acc, n) => acc + n.totalSize, 0);
        const rowFraction = rowTotal / totalBytes;
        return (
          <div
            key={ri}
            className="flex flex-wrap gap-1"
            style={{
              /* Row height = its share of the total bytes, mapped onto
               * a 320 px treemap canvas. minHeight 40 px ensures both
               * the name (11 px) and size (10 px) lines fit even when
               * a row carries a tiny fraction of the total.
               *
               * flex-wrap lets cells flow to a new line if their
               * combined minWidth (64 px each) exceeds the container —
               * matters most on narrow windows or rows with many tiny
               * folders. Without wrap, cells would clip past the right
               * edge once min-widths summed past 100 %. */
              minHeight: "40px",
              height: `${Math.max(40, rowFraction * 320)}px`,
            }}
          >
            {row.map((n) => {
              const colorIntensity = Math.min(
                0.9,
                0.3 + (n.totalSize / totalBytes) * 4,
              );
              return (
                <button
                  key={n.name}
                  type="button"
                  onClick={() => onDrill(n.name, n.isDir)}
                  disabled={!n.isDir}
                  title={`${n.name} — ${formatBytes(n.totalSize)}`}
                  className={`flex min-w-0 flex-col justify-center overflow-hidden rounded px-1.5 py-0.5 text-[10px] text-white transition ${
                    n.isDir ? "cursor-pointer hover:opacity-80" : "cursor-default"
                  }`}
                  /* flex-grow with size as weight: row width is shared
                   * proportional to size while gaps come out of the
                   * grow budget — no overflow even with gap-1 between
                   * cells. Avoids the classic "% widths sum to 100 +
                   * (n-1)*gap → last cell pushed past the right edge"
                   * bug that made the treemap clip on wide rows.
                   *
                   * minWidth (64 px) trades a small amount of strict
                   * proportionality for text readability: without it,
                   * many tiny folders in one row each got a ~6 px slice
                   * and both name + size were truncated to a single
                   * character — visually a colored bar with no label.
                   * 64 px fits short names + a size like "12 KB". Cells
                   * larger than minWidth still distribute the extra
                   * space proportionally via flex-grow, so the rough
                   * "bigger folder = bigger area" still holds. */
                  style={{
                    flexGrow: n.totalSize,
                    flexBasis: 0,
                    minWidth: "64px",
                    backgroundColor: `oklch(0.45 ${0.13 * colorIntensity} 250)`,
                  }}
                >
                  <div className="truncate text-[11px] font-medium leading-tight">
                    {n.name}
                  </div>
                  <div className="truncate font-mono text-[10px] leading-tight opacity-85">
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

// formatBytes moved to lib/format.ts.
