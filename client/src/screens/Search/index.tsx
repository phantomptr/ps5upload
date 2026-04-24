import { useEffect, useRef, useState } from "react";
import {
  Search as SearchIcon,
  Loader2,
  File as FileIcon,
  Folder,
  X,
} from "lucide-react";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { searchPS5, type SearchHit, type SearchProgress } from "../../api/ps5";
import { PageHeader, ErrorCard, Button } from "../../components";
import { useTr } from "../../state/lang";

const SIZE_OPTIONS: { label: string; bytes: number }[] = [
  { label: "any size", bytes: 0 },
  { label: "> 100 MB", bytes: 100 * 1024 * 1024 },
  { label: "> 1 GB", bytes: 1024 * 1024 * 1024 },
  { label: "> 10 GB", bytes: 10 * 1024 * 1024 * 1024 },
];

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

export default function SearchScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const [pattern, setPattern] = useState("");
  const [minSize, setMinSize] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{
    hits: SearchHit[];
    scanned: number;
    truncated: boolean;
    cancelled: boolean;
  } | null>(null);
  // Live progress during the fan-out. Cleared when a run finishes so
  // the progress strip disappears once `result` takes over.
  const [progress, setProgress] = useState<SearchProgress | null>(null);
  // Holds the in-flight search's AbortController so the cancel button
  // can stop it. Kept in a ref (not state) because we only read it
  // synchronously from click handlers — no re-render needed.
  const abortRef = useRef<AbortController | null>(null);

  // Cancel any in-flight search on unmount. Without this, navigating
  // away from the Search tab mid-scan lets the recursive FS_LIST_DIR
  // fan-out keep hammering the PS5 mgmt port in the background — the
  // React state writes become no-ops (unmounted), but the network
  // traffic and the payload's work continue until the walk finishes.
  // Wasteful on large trees and can delay a follow-up mgmt call.
  useEffect(() => {
    return () => {
      abortRef.current?.abort();
    };
  }, []);

  const run = async () => {
    if (!host?.trim() || !pattern.trim()) return;
    // New controller per run so an old (already-aborted) controller
    // can't leak into a fresh search.
    const controller = new AbortController();
    abortRef.current = controller;
    setLoading(true);
    setError(null);
    setResult(null);
    setProgress({ scanned: 0, hits: 0, currentPath: "" });
    try {
      const res = await searchPS5(
        `${host}:${PS5_PAYLOAD_PORT}`,
        pattern,
        minSize,
        setProgress,
        undefined,
        controller.signal
      );
      setResult(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setResult(null);
    } finally {
      setLoading(false);
      setProgress(null);
      abortRef.current = null;
    }
  };

  const cancel = () => {
    abortRef.current?.abort();
  };

  return (
    <div className="p-6">
      <PageHeader
        icon={SearchIcon}
        title={tr("search", undefined, "Search")}
        description={tr(
          "search_description",
          undefined,
          "Searches every writable drive on your PS5. Use * and ? as wildcards (e.g. *.pkg, PPSA?????)",
        )}
      />

      <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
        <div className="flex items-center gap-2">
          <input
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && run()}
            placeholder="*.pkg  /  eboot.bin  /  PPSA*"
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)]"
          />
          <select
            value={minSize}
            onChange={(e) => setMinSize(Number(e.target.value))}
            className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm"
          >
            {SIZE_OPTIONS.map((o) => (
              <option key={o.bytes} value={o.bytes}>
                {o.label}
              </option>
            ))}
          </select>
          {loading ? (
            <Button
              variant="secondary"
              size="md"
              leftIcon={<X size={14} />}
              onClick={cancel}
              title={tr("search_stop_tooltip", undefined, "Stop the current search")}
            >
              {tr("search_stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="md"
              leftIcon={<SearchIcon size={14} />}
              onClick={run}
              disabled={!pattern.trim() || !host?.trim()}
            >
              {tr("search", undefined, "Search")}
            </Button>
          )}
        </div>
      </section>

      {error && (
        <div className="mb-4">
          <ErrorCard
            title={tr("search_failed", undefined, "Search failed")}
            detail={error}
          />
        </div>
      )}

      {loading && progress && (
        <div className="mb-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
          <div className="flex items-center gap-2">
            <Loader2
              size={14}
              className="animate-spin text-[var(--color-accent)]"
            />
            <span className="font-medium">Searching</span>
            <span className="text-xs text-[var(--color-muted)]">
              {progress.scanned.toLocaleString()} entr
              {progress.scanned === 1 ? "y" : "ies"} scanned ·{" "}
              {progress.hits.toLocaleString()} match
              {progress.hits === 1 ? "" : "es"} so far
            </span>
          </div>
          {progress.currentPath && (
            <div className="mt-1 truncate font-mono text-xs text-[var(--color-muted)]">
              in {progress.currentPath}
            </div>
          )}
        </div>
      )}

      {result && result.hits.length === 0 && !loading && (
        <div className="rounded-md border border-dashed border-[var(--color-border)] p-4 text-center text-xs text-[var(--color-muted)]">
          {result.cancelled
            ? `Search stopped. Scanned ${result.scanned.toLocaleString()} entries before you cancelled.`
            : `No matches. Scanned ${result.scanned.toLocaleString()} entries.`}
          {result.truncated && " Stopped at 100,000 entries — try a narrower pattern."}
        </div>
      )}

      {result && result.hits.length > 0 && (
        <>
          <div className="mb-2 text-xs text-[var(--color-muted)]">
            {result.hits.length.toLocaleString()} match
            {result.hits.length === 1 ? "" : "es"} · scanned{" "}
            {result.scanned.toLocaleString()} entries
            {result.cancelled && " · you stopped the search"}
            {result.truncated && " · stopped at 100k"}
          </div>
          <ul className="grid gap-1">
            {result.hits.map((h, i) => {
              const Icon = h.kind === "dir" ? Folder : FileIcon;
              return (
                <li
                  key={`${h.path}-${i}`}
                  className="flex items-center gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-sm"
                >
                  <Icon size={14} className="shrink-0 text-[var(--color-muted)]" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-mono text-sm">{h.name}</div>
                    <div className="truncate font-mono text-xs text-[var(--color-muted)]">
                      {h.path}
                    </div>
                  </div>
                  <span className="shrink-0 text-xs text-[var(--color-muted)] tabular-nums">
                    {h.size > 0 ? formatBytes(h.size) : "—"}
                  </span>
                </li>
              );
            })}
          </ul>
        </>
      )}
    </div>
  );
}
