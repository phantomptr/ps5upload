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
// Direct import to avoid the barrel's circular-dep warning at build.
import { usePrompt } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";

/** Size filter options. `labelKey` resolves through `tr()` at render
 *  time; `labelFallback` is the English text used when the lang file
 *  doesn't have the key yet. Keeping a separate label-and-key form
 *  here (rather than wrapping in tr at module load) is necessary
 *  because tr requires React context and module-level constants are
 *  evaluated outside any component. */
const SIZE_OPTIONS: { labelKey: string; labelFallback: string; bytes: number }[] = [
  { labelKey: "search_size_any", labelFallback: "any size", bytes: 0 },
  { labelKey: "search_size_100mb", labelFallback: "> 100 MB", bytes: 100 * 1024 * 1024 },
  { labelKey: "search_size_1gb", labelFallback: "> 1 GB", bytes: 1024 * 1024 * 1024 },
  { labelKey: "search_size_10gb", labelFallback: "> 10 GB", bytes: 10 * 1024 * 1024 * 1024 },
];

// formatBytes moved to lib/format.ts.

interface SavedSearch {
  id: string;
  name: string;
  pattern: string;
  minSize: number;
}

const SAVED_SEARCHES_KEY = "ps5upload.saved_searches.v1";

function loadSavedSearches(): SavedSearch[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(SAVED_SEARCHES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (s): s is SavedSearch =>
        typeof s?.id === "string" &&
        typeof s?.name === "string" &&
        typeof s?.pattern === "string",
    );
  } catch {
    return [];
  }
}

function persistSavedSearches(s: SavedSearch[]) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SAVED_SEARCHES_KEY, JSON.stringify(s));
  } catch {
    // best-effort
  }
}

export default function SearchScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const [pattern, setPattern] = useState("");
  const [minSize, setMinSize] = useState(0);
  const [saved, setSaved] = useState<SavedSearch[]>(() => loadSavedSearches());
  const { prompt: promptDialog, dialog: promptDialogNode } = usePrompt();
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
        // trim() to match the gate above and every other screen — a stored
        // host with stray whitespace would otherwise form a bad address
        // even though the Search button was enabled.
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
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

  async function saveCurrent() {
    if (!pattern.trim()) return;
    const name = await promptDialog({
      title: tr(
        "search_save_prompt",
        undefined,
        "Name this search (e.g. Big PKGs)",
      ),
      defaultValue: pattern,
      okLabel: tr("save", undefined, "Save"),
    });
    if (!name?.trim()) return;
    const id =
      typeof crypto !== "undefined" && "randomUUID" in crypto
        ? crypto.randomUUID()
        : `s_${Math.random().toString(36).slice(2, 10)}`;
    const next = [
      { id, name: name.trim(), pattern, minSize },
      ...saved.filter((s) => s.name !== name.trim()),
    ].slice(0, 12);
    setSaved(next);
    persistSavedSearches(next);
  }

  function applySaved(s: SavedSearch) {
    setPattern(s.pattern);
    setMinSize(s.minSize);
  }

  function removeSaved(id: string) {
    const next = saved.filter((s) => s.id !== id);
    setSaved(next);
    persistSavedSearches(next);
  }

  return (
    <div className="p-6">
      {promptDialogNode}
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
                {tr(o.labelKey, undefined, o.labelFallback)}
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
        {(saved.length > 0 || pattern.trim()) && (
          <div className="mt-2 flex flex-wrap items-center gap-1.5 text-[11px]">
            {pattern.trim() && (
              <button
                type="button"
                onClick={saveCurrent}
                className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 hover:bg-[var(--color-surface-3)]"
              >
                {tr("search_save", undefined, "Save current")}
              </button>
            )}
            {saved.map((s) => (
              <span
                key={s.id}
                className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] py-0.5 pl-2 pr-1"
              >
                <button
                  type="button"
                  onClick={() => applySaved(s)}
                  title={`${s.pattern} (min ${s.minSize})`}
                  className="hover:text-[var(--color-accent)]"
                >
                  {s.name}
                </button>
                <button
                  type="button"
                  onClick={() => removeSaved(s.id)}
                  className="px-1 text-[var(--color-muted)] hover:text-[var(--color-bad)]"
                  title={tr("search_remove", undefined, "Remove saved search")}
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
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
            <span className="font-medium">
              {tr("search_searching", undefined, "Searching")}
            </span>
            <span className="text-xs text-[var(--color-muted)]">
              {progress.scanned.toLocaleString()}{" "}
              {tr("search_entr", undefined, "entr")}
              {progress.scanned === 1 ? "y" : "ies"}{" "}
              {tr("search_scanned_progress", undefined, "scanned ·")}{" "}
              {progress.hits.toLocaleString()}{" "}
              {tr("search_match_progress", undefined, "match")}
              {progress.hits === 1 ? "" : "es"}{" "}
              {tr("search_so_far", undefined, "so far")}
            </span>
          </div>
          {progress.currentPath && (
            <div className="mt-1 truncate font-mono text-xs text-[var(--color-muted)]">
              {tr("search_in", undefined, "in")} {progress.currentPath}
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
          <div className="mb-2 flex items-center gap-3 text-xs text-[var(--color-muted)]">
            <span>
              {result.hits.length.toLocaleString()}{" "}
              {tr("search_match_result", undefined, "match")}
              {result.hits.length === 1 ? "" : "es"}{" "}
              {tr("search_scanned_result", undefined, "· scanned")}{" "}
              {result.scanned.toLocaleString()}{" "}
              {tr("search_entries_result", undefined, "entries")}
              {result.cancelled && " · you stopped the search"}
              {result.truncated && " · stopped at 100k"}
            </span>
            <button
              type="button"
              onClick={() => exportSearchResults(result.hits, "csv")}
              className="ml-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 text-[10px] hover:bg-[var(--color-surface-3)]"
            >
              {tr("search_export_csv", undefined, "Export CSV")}
            </button>
            <button
              type="button"
              onClick={() => exportSearchResults(result.hits, "json")}
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 text-[10px] hover:bg-[var(--color-surface-3)]"
            >
              {tr("search_export_json", undefined, "Export JSON")}
            </button>
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

/** Save the current hits to disk via the Tauri save dialog. CSV uses
 *  RFC 4180 quoting (double-quote both wrappers and embedded quotes);
 *  JSON serializes the entire SearchHit shape. */
async function exportSearchResults(
  hits: SearchHit[],
  format: "csv" | "json",
) {
  const { save } = await import("@tauri-apps/plugin-dialog");
  const { writeTextFileToPath } = await import("../../lib/saveTextFile");
  const dest = await save({
    defaultPath: `ps5upload-search-${Date.now()}.${format}`,
    filters: [{ name: format.toUpperCase(), extensions: [format] }],
  });
  if (!dest || typeof dest !== "string") return;
  if (format === "json") {
    await writeTextFileToPath(dest, JSON.stringify(hits, null, 2));
    return;
  }
  // CSV
  const esc = (v: string | number) => {
    const s = String(v);
    if (/[",\n\r]/.test(s)) return `"${s.replace(/"/g, '""')}"`;
    return s;
  };
  const rows = ["path,name,size,kind"];
  for (const h of hits) {
    rows.push([h.path, h.name, h.size, h.kind].map(esc).join(","));
  }
  await writeTextFileToPath(dest, rows.join("\n"));
}
