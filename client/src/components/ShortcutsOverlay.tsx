import { useEffect, useState } from "react";
import { Keyboard, X } from "lucide-react";

import { useTr } from "../state/lang";
import { isMobile } from "../lib/platform";

/**
 * Press `?` to display every keybinding the app supports. Modeled
 * after GitHub's `?` overlay — globally available, dismisses on
 * Escape or click-outside.
 *
 * The shortcuts list is hand-curated rather than pulled from a
 * registry. The set is small enough that maintaining a flat array is
 * less code than building a registration mechanism, and an out-of-
 * date list is easy to spot at a glance.
 *
 * Trigger semantics:
 *   - `?` (Shift+/) opens the overlay
 *   - Suppressed when the user is typing in an input/textarea so
 *     "?" can be typed into form fields normally
 */

interface Shortcut {
  keys: string;
  /** i18n key for the description (English literal lives in `description`). */
  descKey: string;
  description: string;
}

const SHORTCUTS: Array<{
  sectionKey: string;
  section: string;
  items: Shortcut[];
}> = [
  {
    sectionKey: "shortcuts_sec_navigation",
    section: "Navigation",
    items: [
      {
        keys: "Cmd/Ctrl + K",
        descKey: "shortcuts_cmd_palette",
        description: "Open command palette",
      },
      { keys: "?", descKey: "shortcuts_help", description: "Show this help" },
      {
        keys: "Esc",
        descKey: "shortcuts_close",
        description: "Close any open modal or overlay",
      },
    ],
  },
  {
    sectionKey: "shortcuts_sec_filesystem",
    section: "File system",
    items: [
      {
        keys: "↑ ↓",
        descKey: "shortcuts_move_selection",
        description: "Move selection in lists / palette",
      },
      {
        keys: "Enter",
        descKey: "shortcuts_activate",
        description: "Open / activate",
      },
    ],
  },
];

export function ShortcutsOverlay() {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  // Touch devices have no `?` key to press and no keyboard shortcuts
  // worth documenting — don't register listeners or render anything.
  // isMobile() is UA-based and stable for the session, so gating the
  // effect body (rather than the hook calls) keeps hook order legal.
  const mobile = isMobile();

  useEffect(() => {
    if (mobile) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "?" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        const target = e.target as HTMLElement | null;
        const tag = target?.tagName?.toLowerCase();
        if (
          tag === "input" ||
          tag === "textarea" ||
          target?.isContentEditable
        ) {
          return;
        }
        e.preventDefault();
        setOpen((v) => !v);
        return;
      }
      if (e.key === "Escape" && open) {
        e.preventDefault();
        setOpen(false);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, mobile]);

  if (mobile || !open) return null;

  return (
    <div
      className="anim-scrim fixed inset-0 z-[60] flex items-center justify-center bg-[var(--overlay-scrim)]"
      onClick={(e) => {
        if (e.target === e.currentTarget) setOpen(false);
      }}
    >
      <div className="anim-pop elev-3 w-[480px] max-w-[90vw] overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]">
        <header className="flex items-center justify-between border-b border-[var(--color-border)] px-4 py-3">
          <div className="flex items-center gap-2">
            <Keyboard size={14} className="text-[var(--color-muted)]" />
            <h2 className="text-sm font-semibold">
              {tr("shortcuts_title", "Keyboard shortcuts")}
            </h2>
          </div>
          <button
            type="button"
            onClick={() => setOpen(false)}
            className="text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            <X size={14} />
          </button>
        </header>
        <div className="max-h-[60vh] overflow-y-auto p-4 text-sm">
          {SHORTCUTS.map((section) => (
            <div key={section.section} className="mb-4 last:mb-0">
              <div className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--color-muted)]">
                {tr(section.sectionKey, section.section)}
              </div>
              <ul className="space-y-1">
                {section.items.map((s) => (
                  <li
                    key={s.keys}
                    className="flex items-center justify-between"
                  >
                    <span className="text-xs">{tr(s.descKey, s.description)}</span>
                    <kbd className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1.5 py-0.5 font-mono text-xs">
                      {s.keys}
                    </kbd>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
