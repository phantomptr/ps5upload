import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Terminal, Play, Pause, Trash2, Copy, Filter } from "lucide-react";
import { klogChunk } from "../../api/ps5";
import { mgmtAddr } from "../../lib/addr";
import { useConnectionStore } from "../../state/connection";
import { Button, EmptyState, ErrorCard } from "../../components";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { writeClipboard } from "../../lib/clipboard";
import { useDocumentVisible } from "../../lib/visibility";

/**
 * Live kernel log viewer with noise filtering.
 *
 * Polls /dev/klog through the payload's KLOG_READ frame every second
 * while playing. Designed to render as a child of the Logs tab shell —
 * owns its own action row and filter panel but not the outer page
 * header.
 *
 * See the noise classifier comments below for why default-show is
 * "everything" and the Sony-noise preset is opt-in.
 */

type Category =
  | "ps5upload"
  | "shellui-crash"
  | "kernel-error"
  | "sony-pf-auth"
  | "sony-bgs-storage"
  | "sony-rnps"
  | "sony-curl"
  | "sony-memory-stats"
  | "sony-framedrop"
  | "sony-tick"
  | "sony-fmem"
  | "sony-other"
  | "kstuff-payload"
  | "etahen-payload"
  | "shellui-info"
  | "other";

const CATEGORY_RULES: Array<{ cat: Category; re: RegExp }> = [
  { cat: "ps5upload", re: /ps5upload|\bpayload\.elf\b/i },
  {
    cat: "shellui-crash",
    re: /SIGSEGV|SIGABRT|SIGBUS|fatal signal|coredump|Native Crash Reporting|page fault.*protection violation/i,
  },
  { cat: "kernel-error", re: /panic|kernel panic|\bBUG\b|kassert/i },
  { cat: "kstuff-payload", re: /\bkstuff\.elf\b/ },
  { cat: "etahen-payload", re: /\betaHEN\b|\betahen\b/i },
  { cat: "sony-pf-auth", re: /\[PFAuthClient\]/ },
  { cat: "sony-bgs-storage", re: /\bBgsStorage\b/ },
  {
    cat: "sony-rnps",
    re: /\bRNPS\b|HERMES TTPoolInstance|TwinTurbo|rnpsjscoverageinfo/,
  },
  { cat: "sony-curl", re: /\[RNPS Curl\]/ },
  {
    cat: "sony-memory-stats",
    re: /\bVM Stats\b|\bLibc Heap Status\b|page table CPU/,
  },
  { cat: "sony-framedrop", re: /\[Performance Warning\].*framedrop/ },
  { cat: "sony-tick", re: /PRINT_TIME_TICK/ },
  { cat: "sony-fmem", re: /\[SceShellCore\]\s+FMEM\b/ },
  {
    cat: "sony-other",
    re: /\bCFFCHECKER\b|\bPFAuth\b|\bSceConsoleFeatureFlagChecker\b|SetBackgroundTransition|\[BootEvent\]|\[Theme\/|SceLoginMgr|LOGIN MGR/,
  },
  { cat: "shellui-info", re: /\[SceShellUI\]|\[PSM\.UI\]/ },
];

const DEFAULT_HIDDEN_CATEGORIES: ReadonlySet<Category> = new Set<Category>();

const CATEGORY_LABEL: Record<Category, string> = {
  ps5upload: "ps5upload",
  "shellui-crash": "ShellUI crash",
  "kernel-error": "Kernel panic/error",
  "sony-pf-auth": "Sony PFAuthClient",
  "sony-bgs-storage": "Sony BgsStorage",
  "sony-rnps": "Sony RNPS framework",
  "sony-curl": "Sony RNPS Curl",
  "sony-memory-stats": "Sony VM/heap stats",
  "sony-framedrop": "Sony framedrop",
  "sony-tick": "Sony tick markers",
  "sony-fmem": "Sony FMEM dump",
  "sony-other": "Sony other",
  "kstuff-payload": "kstuff payload",
  "etahen-payload": "etaHEN payload",
  "shellui-info": "ShellUI info",
  other: "Other / uncategorised",
};

const CATEGORY_BORDER: Partial<Record<Category, string>> = {
  ps5upload: "border-l-2 border-l-[var(--color-accent)]",
  "shellui-crash": "border-l-2 border-l-[var(--color-bad)] bg-[var(--color-bad)]/5",
  "kernel-error": "border-l-2 border-l-[var(--color-bad)] bg-[var(--color-bad)]/5",
  "kstuff-payload": "border-l-2 border-l-[var(--color-warn)]",
  "etahen-payload": "border-l-2 border-l-[var(--color-warn)]",
};

function classify(line: string): Category {
  for (const { cat, re } of CATEGORY_RULES) {
    if (re.test(line)) return cat;
  }
  return "other";
}

export default function KernelLogPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [entries, setEntries] = useState<
    Array<{ text: string; cat: Category }>
  >([]);
  const [playing, setPlaying] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hiddenCats, setHiddenCats] = useState<Set<Category>>(
    () => new Set(DEFAULT_HIDDEN_CATEGORIES),
  );
  const [showFilters, setShowFilters] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const userScrolledRef = useRef(false);
  const [userScrolled, setUserScrolled] = useState(false);
  // Pause polling while the window is hidden — same pattern as the
  // Dashboard pollers. /dev/klog reads while minimized are pure waste,
  // and the buffer is drained on the next visible tick anyway.
  // (`docVisible` to avoid clashing with the filtered-lines `visible`.)
  const docVisible = useDocumentVisible();

  // The klog stream belongs to ONE console. Drop the scrollback when the
  // tab switches — without this, console A's lines stay on screen and
  // console B's lines append after them, producing an unreadable mix.
  useEffect(() => {
    setEntries([]);
    setError(null);
  }, [host]);

  const tick = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    const tickHost = host;
    try {
      const chunk = await klogChunk(mgmtAddr(tickHost.trim()), 16 * 1024);
      // Stale-host guard: a chunk that lands after a tab switch must not
      // leak the previous console's lines into the fresh scrollback.
      if (useConnectionStore.getState().host !== tickHost) return;
      if (chunk && chunk.length > 0) {
        setEntries((prev) => {
          const newLines = chunk
            .split(/\r?\n/)
            .filter((l) => l.length > 0)
            .map((text) => ({ text, cat: classify(text) }));
          // A non-empty chunk can still yield zero lines (e.g. bare
          // newlines). Returning `prev` unchanged keeps the state
          // referentially stable, so the visible/counts useMemo and
          // the 5000-row list diff don't re-run on idle ticks.
          if (newLines.length === 0) return prev;
          const next = [...prev, ...newLines];
          return next.length > 5000 ? next.slice(-5000) : next;
        });
        setError(null);
      }
    } catch (e) {
      if (useConnectionStore.getState().host !== tickHost) return;
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [host, payloadStatus]);

  useEffect(() => {
    if (!playing || !docVisible) return;
    tick();
    const id = window.setInterval(tick, 1000);
    return () => window.clearInterval(id);
  }, [playing, docVisible, tick]);

  const { visible, counts } = useMemo(() => {
    const counts: Partial<Record<Category, number>> = {};
    const visible: Array<{ text: string; cat: Category; idx: number }> = [];
    entries.forEach((e, idx) => {
      counts[e.cat] = (counts[e.cat] ?? 0) + 1;
      if (!hiddenCats.has(e.cat)) {
        visible.push({ ...e, idx });
      }
    });
    return { visible, counts };
  }, [entries, hiddenCats]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el || userScrolledRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [visible]);

  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    const next = distanceFromBottom > 80;
    userScrolledRef.current = next;
    if (next !== userScrolled) setUserScrolled(next);
  }

  function jumpToBottom() {
    const el = scrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
      userScrolledRef.current = false;
      setUserScrolled(false);
    }
  }

  function toggleCat(cat: Category) {
    setHiddenCats((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat);
      else next.add(cat);
      return next;
    });
  }

  function showAll() {
    setHiddenCats(new Set());
  }

  function hideSonyDefaults() {
    setHiddenCats(
      new Set<Category>([
        "sony-pf-auth",
        "sony-bgs-storage",
        "sony-rnps",
        "sony-curl",
        "sony-memory-stats",
        "sony-framedrop",
        "sony-tick",
        "sony-fmem",
        "sony-other",
        "shellui-info",
      ]),
    );
  }

  async function copyVisible() {
    if (visible.length === 0) return;
    const ok = await writeClipboard(visible.map((v) => v.text).join("\n"));
    if (ok) {
      pushNotification(
        "success",
        withConsolePrefix(host, "Kernel log copied"),
        {
          body: `${visible.length} visible line${visible.length === 1 ? "" : "s"} on the clipboard. (${entries.length - visible.length} filtered.)`,
        },
      );
    } else {
      pushNotification("warning", "Copy failed", {
        body: "Clipboard access denied.",
      });
    }
  }

  if (payloadStatus !== "up") {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <EmptyState
          fill
          icon={Terminal}
          message={tr(
            "klog_no_payload",
            undefined,
            "Connect to your PS5 first — kernel log is read live.",
          )}
        />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Top action row — Filters/Play/Pause/Copy/Clear. Includes a
          "showing N of M" count so the eye-down-from-tabs flow shows
          state immediately. */}
      <div className="mb-3 flex items-center justify-between gap-2">
        <span className="text-xs tabular-nums text-[var(--color-muted)]">
          {tr("kernellog_showing", undefined, "showing")} {visible.length}{" "}
          {tr("kernellog_of", undefined, "of")} {entries.length}
        </span>
        <div className="flex items-center gap-2">
          <Button
            variant={showFilters ? "primary" : "secondary"}
            size="sm"
            leftIcon={<Filter size={12} />}
            onClick={() => setShowFilters((v) => !v)}
          >
            {tr("klog_filters", undefined, "Filters")}
            {hiddenCats.size > 0 && (
              <span className="ml-1 rounded bg-[var(--color-surface)] px-1 text-xs text-[var(--color-muted)]">
                −{hiddenCats.size}
              </span>
            )}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={playing ? <Pause size={12} /> : <Play size={12} />}
            onClick={() => setPlaying((v) => !v)}
            disabled={payloadStatus !== "up"}
          >
            {playing
              ? tr("klog_pause", undefined, "Pause")
              : tr("klog_play", undefined, "Play")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Copy size={12} />}
            onClick={copyVisible}
            disabled={visible.length === 0}
          >
            {tr("klog_copy", undefined, "Copy")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Trash2 size={12} />}
            onClick={() => setEntries([])}
            disabled={entries.length === 0}
          >
            {tr("klog_clear", undefined, "Clear")}
          </Button>
        </div>
      </div>

      {showFilters && (
        <div className="mb-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
          <div className="mb-1.5 flex items-center justify-between">
            <span className="font-semibold">
              {tr("kernellog_categories", undefined, "Categories")}
            </span>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={hideSonyDefaults}
                className="text-xs underline-offset-2 hover:underline"
              >
                {tr("kernellog_hide_sony_noise", undefined, "Hide Sony noise")}
              </button>
              <button
                type="button"
                onClick={showAll}
                className="text-xs underline-offset-2 hover:underline"
              >
                {tr(
                  "kernellog_show_all_default",
                  undefined,
                  "Show all (default)",
                )}
              </button>
            </div>
          </div>
          <div className="grid grid-cols-1 gap-x-3 gap-y-1 sm:grid-cols-2 md:grid-cols-3">
            {(Object.keys(CATEGORY_LABEL) as Category[]).map((cat) => {
              const count = counts[cat] ?? 0;
              const hidden = hiddenCats.has(cat);
              return (
                <label
                  key={cat}
                  className={`flex cursor-pointer items-center gap-1.5 ${
                    count === 0 ? "opacity-50" : ""
                  }`}
                >
                  <input
                    type="checkbox"
                    checked={!hidden}
                    onChange={() => toggleCat(cat)}
                    className="mt-[1px]"
                  />
                  <span className="flex-1 truncate">{CATEGORY_LABEL[cat]}</span>
                  <span className="tabular-nums text-[var(--color-muted)]">
                    {count}
                  </span>
                </label>
              );
            })}
          </div>
          <div className="mt-2 text-xs text-[var(--color-muted)]">
            {tr(
              "kernellog_default_hidden_explainer",
              undefined,
              "Default-hidden categories are Sony's own subsystems that emit constantly on every jailbroken PS5 regardless of what payload is running. They aren't caused by ps5upload. The crash, ps5upload, and other-payload categories are always visible by default.",
            )}
          </div>
        </div>
      )}
      {error && (
        <div className="mb-2">
          <ErrorCard title={error} />
        </div>
      )}
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="flex-1 overflow-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-mono text-xs leading-tight"
      >
        {visible.length === 0 ? (
          <div className="text-[var(--color-muted)]">
            {entries.length === 0
              ? tr(
                  "klog_empty",
                  undefined,
                  "Waiting for kernel log output… (the buffer empties between reads, so messages from before you opened this tab won't appear here).",
                )
              : `All ${entries.length} buffered line${entries.length === 1 ? "" : "s"} are hidden by your filters. Open the Filters panel to enable more categories.`}
          </div>
        ) : (
          visible.map((v) => (
            <div
              key={v.idx}
              className={`whitespace-pre-wrap break-all px-1 hover:bg-[var(--color-surface-2)] ${
                CATEGORY_BORDER[v.cat] ?? ""
              }`}
              title={CATEGORY_LABEL[v.cat]}
            >
              {v.text}
            </div>
          ))
        )}
      </div>
      {userScrolled && (
        <div className="mt-2 flex justify-end">
          <button
            type="button"
            onClick={jumpToBottom}
            className="rounded-md bg-[var(--color-accent)] px-2 py-1 text-xs text-[var(--color-accent-contrast)] transition-opacity hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
          >
            {tr("klog_jump_bottom", undefined, "Jump to latest")}
          </button>
        </div>
      )}
    </div>
  );
}
