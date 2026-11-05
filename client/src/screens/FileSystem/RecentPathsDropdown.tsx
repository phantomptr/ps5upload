import { useMemo, useRef, useState } from "react";
import { ChevronRight, Star, StarOff } from "lucide-react";

import { useTr } from "../../state/lang";
import { useRecentPathsStore } from "../../state/recentPaths";
import { useFitOnScreen } from "../../components/useFitOnScreen";

/** The open list of the Recent dropdown. An opaque raised surface (the
 *  app's floating-menu glass), stacked above the file list: on the plain
 *  `--color-surface` tint, a 4% wash, the rows under it showed through. */
export function RecentPathsPanel({
  paths,
  onPick,
  onRemove,
  onClear,
  console: pinned,
}: {
  paths: string[];
  onPick: (p: string) => void;
  onRemove: (p: string) => void;
  onClear: () => void;
  /** The folders pinned on the console's own Files page (a helper that shares them). */
  console?: ConsolePinsGroup;
}) {
  const tr = useTr();
  const ref = useRef<HTMLDivElement>(null);
  const fit = useFitOnScreen(ref, true);
  return (
    <div
      ref={ref}
      data-testid="fs-recent-list"
      className="glass-float elev-2 absolute end-0 top-full z-40 mt-1 w-72 max-w-[calc(100vw-1rem)] overflow-y-auto rounded-[var(--radius-card)]"
      style={fit}
    >
      {pinned && <ConsolePinnedGroup group={pinned} onPick={onPick} />}
      {paths.length > 0 && (
        <ul className="max-h-60 overflow-y-auto py-1">
          {paths.map((p) => (
            <li key={p} className="flex items-center">
              <button
                type="button"
                onClick={() => onPick(p)}
                className="min-w-0 flex-1 truncate px-2 py-1 text-start font-mono text-xs hover:bg-[var(--color-surface-3)]"
                title={p}
              >
                {p}
              </button>
              <button
                type="button"
                onClick={() => onRemove(p)}
                className="px-1 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-bad)]"
                title={tr("fs_recent_remove", undefined, "Forget this entry")}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
      {paths.length > 0 && (
        <button
          type="button"
          onClick={onClear}
          className="block w-full border-t border-[var(--color-border)] px-2 py-1 text-start text-xs text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          {tr("fs_recent_clear", undefined, "Clear all")}
        </button>
      )}
    </div>
  );
}

/** "Pinned on console": what the dropdown needs to show and change the console's pins. */
export interface ConsolePinsGroup {
  pins: string[];
  /** The folder shown is pinned on the console. */
  pinned: boolean;
  onTogglePin: () => void;
}

/** The console's pinned folders (a click opens one here) and pin / unpin for the folder shown. */
export function ConsolePinnedGroup({
  group,
  onPick,
}: {
  group: ConsolePinsGroup;
  onPick: (p: string) => void;
}) {
  const tr = useTr();
  const Icon = group.pinned ? StarOff : Star;
  return (
    <div
      data-testid="fs-console-pins"
      className="border-b border-[var(--color-border)] py-1"
    >
      <div className="px-2 pb-0.5 pt-1 text-[10px] uppercase tracking-wider text-[var(--color-muted)]">
        {tr("fs_console_pinned", undefined, "Pinned on console")}
      </div>
      <ul className="max-h-40 overflow-y-auto">
        {group.pins.map((p) => (
          <li key={p}>
            <button
              type="button"
              onClick={() => onPick(p)}
              className="flex w-full min-w-0 items-center gap-1.5 truncate px-2 py-1 text-start font-mono text-xs hover:bg-[var(--color-surface-3)]"
              title={p}
            >
              <Star size={11} className="shrink-0 text-[var(--color-accent)]" />
              <span className="truncate">{p}</span>
            </button>
          </li>
        ))}
      </ul>
      <button
        type="button"
        data-testid="fs-console-pin-toggle"
        onClick={group.onTogglePin}
        className="flex w-full items-center gap-1.5 px-2 py-1 text-start text-xs text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
      >
        <Icon size={12} className="shrink-0" />
        {group.pinned
          ? tr(
              "fs_console_unpin",
              undefined,
              "Unpin this folder on the console",
            )
          : tr("fs_console_pin", undefined, "Pin this folder on the console")}
      </button>
    </div>
  );
}

/** Compact "recent paths" dropdown that lives next to the breadcrumb
 *  trail. Hidden when the recents list is empty (no point showing an
 *  empty button). Positioned to the right of the breadcrumbs so it
 *  doesn't push the active path off-screen on small windows. */
export function RecentPathsDropdown({
  onPick,
  currentPath,
  console: pinned,
  onOpen,
}: {
  onPick: (p: string) => void;
  currentPath: string;
  /** "Pinned on console" (only with a helper that shares the console's pins). */
  console?: ConsolePinsGroup;
  /** The list opened (read the console's pins again). */
  onOpen?: () => void;
}) {
  const tr = useTr();
  const paths = useRecentPathsStore((s) => s.paths);
  const remove = useRecentPathsStore((s) => s.remove);
  const clear = useRecentPathsStore((s) => s.clear);
  const [open, setOpen] = useState(false);

  // Filter out the current path — no value in offering "go where you
  // already are." Keeps the list focused on actual destinations.
  const filtered = useMemo(
    () => paths.filter((p) => p !== currentPath),
    [paths, currentPath],
  );

  if (filtered.length === 0 && !pinned) return null;

  return (
    <div className="relative ms-auto">
      <button
        type="button"
        onClick={() => {
          if (!open) onOpen?.();
          setOpen((v) => !v);
        }}
        title={tr("fs_recent_tooltip", undefined, "Recent destinations")}
        aria-expanded={open}
        className="flex items-center gap-1 rounded-full px-1.5 py-0.5 text-xs uppercase tracking-wider text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
      >
        {tr("fs_recent", "Recent")}
        <ChevronRight
          size={10}
          className={`transition-transform ${open ? "rotate-90" : "rtl:rotate-180"}`}
        />
      </button>
      {open && (
        <RecentPathsPanel
          paths={filtered}
          onPick={(p) => {
            onPick(p);
            setOpen(false);
          }}
          console={pinned}
          onRemove={remove}
          onClear={() => {
            clear();
            setOpen(false);
          }}
        />
      )}
    </div>
  );
}
