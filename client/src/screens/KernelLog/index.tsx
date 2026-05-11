import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Terminal,
  Play,
  Pause,
  Trash2,
  Copy,
  Filter,
} from "lucide-react";
import { klogChunk } from "../../api/ps5";
import { useConnectionStore } from "../../state/connection";
import { PageHeader, Button, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";

/**
 * Live kernel log viewer with **noise filtering**.
 *
 * Polls /dev/klog through the payload's KLOG_READ frame every second
 * while playing. The PS5's kernel log on a jailbroken console is
 * dominated by Sony's own subsystems retrying broken operations
 * (PSN auth failing, BgsStorage queries with system user, RNPS curl
 * 400s, periodic FMEM/VM-stat dumps, framedrop counters). Out of the
 * box you'll see thousands of "errors" per minute that have nothing
 * to do with ps5upload.
 *
 * The filter strips that noise by default so the lines you see are
 * actually worth reading. Toggle "Show Sony noise" if you need the
 * raw stream (e.g. debugging a Sony API path or comparing payload
 * activity timing against Sony's reactions).
 */

/* ── Noise classifier ────────────────────────────────────────────
 * Each category gets a regex + a default-show flag. The classifier
 * picks the FIRST matching category, so order matters — most
 * specific patterns first.
 *
 * "Sony noise" = anything emitted by Sony's own subsystems that
 * happens on every jailbroken console regardless of what payload is
 * running. Hidden by default because seeing it ≠ a problem to fix.
 *
 * "Other payload" = lines from non-ps5upload payloads on this same
 * console (kstuff bind-mount failures, etc.). Shown by default with
 * a label so you can distinguish from our payload's behavior. */
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
  // Our own + crash signal — always show.
  { cat: "ps5upload", re: /ps5upload|\bpayload\.elf\b/i },
  {
    cat: "shellui-crash",
    re: /SIGSEGV|SIGABRT|SIGBUS|fatal signal|coredump|Native Crash Reporting|page fault.*protection violation/i,
  },
  { cat: "kernel-error", re: /panic|kernel panic|\bBUG\b|kassert/i },

  // Other PS5 payloads on the same console — shown by default but tagged.
  { cat: "kstuff-payload", re: /\bkstuff\.elf\b/ },
  { cat: "etahen-payload", re: /\betaHEN\b|\betahen\b/i },

  // Sony noise — hidden by default.
  { cat: "sony-pf-auth", re: /\[PFAuthClient\]/ },
  { cat: "sony-bgs-storage", re: /\bBgsStorage\b/ },
  { cat: "sony-rnps", re: /\bRNPS\b|HERMES TTPoolInstance|TwinTurbo|rnpsjscoverageinfo/ },
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

  // Generic ShellUI info — shown by default but lower priority than crashes.
  { cat: "shellui-info", re: /\[SceShellUI\]|\[PSM\.UI\]/ },
];

/* Default: show everything. The filter panel is opt-in — open it
 * and toggle categories off when you want to silence Sony's normal
 * jailbreak chatter and focus on a specific class of issue. */
const DEFAULT_HIDDEN_CATEGORIES: ReadonlySet<Category> = new Set<Category>();

const CATEGORY_LABEL: Record<Category, string> = {
  "ps5upload": "ps5upload",
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
  "other": "Other / uncategorised",
};

/** Tailwind color hint per category — used for left-border accent.
 *  Crashes get red; our payload gets blue; other payloads get amber;
 *  everything else inherits the surface color (no accent). */
const CATEGORY_BORDER: Partial<Record<Category, string>> = {
  "ps5upload": "border-l-2 border-l-blue-500",
  "shellui-crash": "border-l-2 border-l-red-500 bg-red-500/5",
  "kernel-error": "border-l-2 border-l-red-500 bg-red-500/5",
  "kstuff-payload": "border-l-2 border-l-amber-500",
  "etahen-payload": "border-l-2 border-l-amber-500",
};

function classify(line: string): Category {
  for (const { cat, re } of CATEGORY_RULES) {
    if (re.test(line)) return cat;
  }
  return "other";
}

export default function KernelLogScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  /* Lines as { text, cat } so we don't re-classify on every render +
   * filter toggle. */
  const [entries, setEntries] = useState<Array<{ text: string; cat: Category }>>([]);
  const [playing, setPlaying] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hiddenCats, setHiddenCats] = useState<Set<Category>>(
    () => new Set(DEFAULT_HIDDEN_CATEGORIES),
  );
  const [showFilters, setShowFilters] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const userScrolledRef = useRef(false);
  const [userScrolled, setUserScrolled] = useState(false);

  const tick = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    try {
      const chunk = await klogChunk(`${host.trim()}:9114`, 16 * 1024);
      if (chunk && chunk.length > 0) {
        setEntries((prev) => {
          const newLines = chunk
            .split(/\r?\n/)
            .filter((l) => l.length > 0)
            .map((text) => ({ text, cat: classify(text) }));
          // Cap retained scrollback at 5000 entries.
          const next = [...prev, ...newLines];
          return next.length > 5000 ? next.slice(-5000) : next;
        });
        setError(null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [host, payloadStatus]);

  useEffect(() => {
    if (!playing) return;
    tick();
    const id = window.setInterval(tick, 1000);
    return () => window.clearInterval(id);
  }, [playing, tick]);

  // Filtered view + counts per category.
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

  // Auto-scroll on new entries unless the user scrolled away.
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
    /* "Hide Sony noise" preset — applied on demand when the user
     * is hunting for a real problem and wants the chatter out of
     * the way. Default is "show all". */
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
    try {
      await navigator.clipboard.writeText(visible.map((v) => v.text).join("\n"));
      pushNotification("success", "Kernel log copied", {
        body: `${visible.length} visible line${visible.length === 1 ? "" : "s"} on the clipboard. (${entries.length - visible.length} filtered.)`,
      });
    } catch (e) {
      pushNotification("warning", "Copy failed", {
        body: e instanceof Error ? e.message : "Clipboard access denied.",
      });
    }
  }

  return (
    <div className="flex h-full flex-col p-6">
      <PageHeader
        icon={Terminal}
        title={tr("klog_title", undefined, "Kernel log")}
        count={visible.length}
        description={tr(
          "klog_description",
          undefined,
          "Live stream of /dev/klog. Shows everything by default; open Filters to hide Sony's routine subsystem chatter while hunting a specific issue.",
        )}
        right={
          <div className="flex items-center gap-2">
            <Button
              variant={showFilters ? "primary" : "secondary"}
              size="sm"
              leftIcon={<Filter size={12} />}
              onClick={() => setShowFilters((v) => !v)}
            >
              {tr("klog_filters", undefined, "Filters")}
              {hiddenCats.size > 0 && (
                <span className="ml-1 rounded bg-[var(--color-surface)] px-1 text-[10px] text-[var(--color-muted)]">
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
        }
      />

      {payloadStatus !== "up" ? (
        <EmptyState
          icon={Terminal}
          message={tr(
            "klog_no_payload",
            undefined,
            "Connect to your PS5 first — kernel log is read live.",
          )}
        />
      ) : (
        <div className="flex min-h-0 flex-1 flex-col">
          {showFilters && (
            <div className="mb-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-[11px]">
              <div className="mb-1.5 flex items-center justify-between">
                <span className="font-semibold">Categories</span>
                <div className="flex gap-2">
                  <button
                    type="button"
                    onClick={hideSonyDefaults}
                    className="text-[10px] underline-offset-2 hover:underline"
                  >
                    Hide Sony noise
                  </button>
                  <button
                    type="button"
                    onClick={showAll}
                    className="text-[10px] underline-offset-2 hover:underline"
                  >
                    Show all (default)
                  </button>
                </div>
              </div>
              <div className="grid grid-cols-2 gap-x-3 gap-y-1 md:grid-cols-3">
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
                      <span className="flex-1 truncate">
                        {CATEGORY_LABEL[cat]}
                      </span>
                      <span className="tabular-nums text-[var(--color-muted)]">
                        {count}
                      </span>
                    </label>
                  );
                })}
              </div>
              <div className="mt-2 text-[10px] text-[var(--color-muted)]">
                Default-hidden categories are Sony's own subsystems that
                emit constantly on every jailbroken PS5 regardless of
                what payload is running. They aren't caused by
                ps5upload. The crash, ps5upload, and other-payload
                categories are always visible by default.
              </div>
            </div>
          )}
          {error && (
            <div className="mb-2 rounded-md border border-[var(--color-bad)] p-2 text-[11px] text-[var(--color-bad)]">
              {error}
            </div>
          )}
          <div
            ref={scrollRef}
            onScroll={handleScroll}
            className="flex-1 overflow-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-mono text-[10px] leading-tight"
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
          <div className="mt-2 flex items-center justify-between text-[10px] text-[var(--color-muted)]">
            <span>
              showing {visible.length} of {entries.length} line
              {entries.length === 1 ? "" : "s"}
              {hiddenCats.size > 0 && (
                <>
                  {" · "}
                  {entries.length - visible.length} hidden by filter
                </>
              )}
            </span>
            {userScrolled && (
              <button
                type="button"
                onClick={jumpToBottom}
                className="rounded-md bg-[var(--color-accent)] px-2 py-1 text-[10px] text-[var(--color-accent-contrast)]"
              >
                {tr("klog_jump_bottom", undefined, "Jump to latest")}
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
