import { useCollectionStore } from "../state/collection";
import { gamePath } from "../lib/gamePage";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ArrowRight, Search } from "lucide-react";

import { useThemeStore } from "../state/theme";
import { selectConsoleByAddress } from "../state/roster";
import { pushNotification } from "../state/notifications";
import { useTr } from "../state/lang";
import { useBetaFeaturesStore } from "../state/betaFeatures";
import { isTauriEnv } from "../lib/tauriEnv";
import { NAV_ITEMS, navItemOffered, navItemVisible, PINNED_NAV_ITEMS } from "../layout/navItems";
import { isMacOrLinuxDesktop } from "../lib/platform";

/**
 * Command palette — Cmd/Ctrl+K. Lists navigation targets and a small
 * set of one-shot actions. Filtered by simple substring match (case-
 * insensitive) over both the label and an optional `keywords` array.
 *
 * Action source-of-truth is a literal array rather than a registry to
 * avoid the indirection of registering commands per-screen — the set
 * is small and changes rarely.
 *
 * Closes on Escape, click-outside, or executing a command. Reset on
 * close so reopening always starts fresh.
 */

/** Extra search words per screen (old names and synonyms). */
const NAV_KEYWORDS: Record<string, string[]> = {
  "/home": ["dashboard", "overview"],
  "/connection": ["host", "ip", "connect"],
  "/install-package": ["pkg"],
  "/games": ["library", "apps", "installed"],
  "/captures": ["screenshots", "videos", "clips"],
  "/volumes": ["disk", "drives"],
  "/files": ["browse", "file system"],
  "/console": ["hardware", "temps", "power"],
  "/payloads": ["homebrew catalog", "payload library", "kstuff", "shadowmount", "etahen"],
  "/tasks": ["transfer log", "jobs"],
  "/bug-report": ["report", "problem", "issue", "discord", "github"],
  "/shell": ["terminal"],
  "/faq": ["help"],
};

interface Command {
  id: string;
  label: string;
  keywords?: string[];
  /** Optional secondary line for context. */
  hint?: string;
  /** Group label for visual sectioning. */
  group: "Navigation" | "Theme" | "Connection" | "Window" | "Games";
  run: () => void | Promise<void>;
}

function useCommands(close: () => void): Command[] {
  const navigate = useNavigate();
  const toggleTheme = useThemeStore((s) => s.toggleTheme);
  const tr = useTr();
  const library = useCollectionStore((s) => s.library);
  const games = useMemo(() => (library ? Object.values(library.games) : []), [library]);

  return useMemo<Command[]>(() => {
    // Nav labels reuse the same i18n keys as Sidebar (which already
    // has the {key, fallback} data-table pattern), so there are no
    // new translations to ship. The english fallback also doubles
    // as a keyword for substring matching since toLowerCase().match
    // covers both the rendered label and the underlying fallback.
    const nav = (
      to: string,
      key: string,
      fallback: string,
      keywords?: string[],
    ): Command => ({
      id: `nav:${to}`,
      label: tr(key, undefined, fallback),
      keywords,
      group: "Navigation",
      hint: to,
      run: () => {
        navigate(to);
        close();
      },
    });

    // Every screen the sidebar has (same labels, same visibility rules), so the palette can
    // never list a screen that is gone or call one by an old name. Extra words help matching.
    const beta = useBetaFeaturesStore.getState().enabled;
    const screens = [...PINNED_NAV_ITEMS, ...NAV_ITEMS].filter(
      (i) =>
        navItemOffered(i, { inBrowser: !isTauriEnv(), macOrLinux: isMacOrLinuxDesktop() }) &&
        navItemVisible(i, beta),
    );
    return [
      ...screens.map((i) => nav(i.to, i.key, i.fallback, NAV_KEYWORDS[i.to])),
      // Tabs worth jumping to directly.
      // Send is desktop-only (the browser's Payloads screen has no Send tab).
      ...(isTauriEnv()
        ? [nav("/payloads?tab=send", "payloads_tab_send", "Send payload", ["send payload"])]
        : []),
      nav("/logs?tab=kernel", "logs_tab_kernel", "Kernel log", ["dmesg", "klog"]),
      {
        id: "theme:toggle",
        label: tr("cmdpalette_toggle_theme", undefined, "Toggle theme"),
        keywords: ["dark", "light", "mode"],
        group: "Theme",
        run: () => {
          toggleTheme();
          close();
        },
      },
      {
        id: "conn:paste-host",
        label: tr(
          "cmdpalette_paste_host",
          undefined,
          "Paste host from clipboard",
        ),
        keywords: ["ip", "address", "connect"],
        group: "Connection",
        hint: tr(
          "cmdpalette_paste_host_hint",
          undefined,
          "Reads navigator.clipboard for an IPv4 address",
        ),
        run: async () => {
          try {
            const text = await navigator.clipboard.readText();
            const match = text.match(/(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})/);
            if (match) {
              selectConsoleByAddress(match[1]);
              pushNotification(
                "info",
                tr("cmdpalette_host_updated", undefined, "Host updated"),
                {
                  body: tr(
                    "cmdpalette_host_updated_body",
                    { ip: match[1] },
                    `Set to ${match[1]} from clipboard.`,
                  ),
                },
              );
            } else {
              pushNotification(
                "warning",
                tr("cmdpalette_no_ip_found", undefined, "No IP found"),
                {
                  body: tr(
                    "cmdpalette_no_ip_body",
                    undefined,
                    "Clipboard didn't contain an IPv4 address.",
                  ),
                },
              );
            }
          } catch {
            pushNotification(
              "warning",
              tr(
                "cmdpalette_clipboard_failed",
                undefined,
                "Clipboard read failed",
              ),
              {
                body: tr(
                  "cmdpalette_clipboard_failed_body",
                  undefined,
                  "Permission denied or unavailable.",
                ),
              },
            );
          }
          close();
        },
      },
      // The Collection's games, each to its page. Listed only when searching.
      ...games.map(
        (g): Command => ({
          id: `game:${g.game_id}`,
          label: g.title,
          keywords: [g.game_id],
          group: "Games",
          hint: g.game_id,
          run: () => {
            navigate(gamePath(g.game_id));
            close();
          },
        }),
      ),
    ];
  }, [navigate, toggleTheme, close, tr, games]);
}

/** The event a button sends to open the palette (it is otherwise opened by
 *  Cmd/Ctrl+K). A window event keeps the palette's state where it is. */
export const OPEN_COMMAND_PALETTE_EVENT = "ps5upload:open-command-palette";

/** Open the command palette from anywhere (the search button in the top bar). */
export function openCommandPalette(): void {
  window.dispatchEvent(new Event(OPEN_COMMAND_PALETTE_EVENT));
}

export function CommandPalette() {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIdx, setActiveIdx] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const activeRef = useRef<HTMLButtonElement | null>(null);

  const close = () => {
    setOpen(false);
    setQuery("");
    setActiveIdx(0);
  };

  const commands = useCommands(close);

  // Global hotkey: Cmd/Ctrl+K. Skip when the user is typing in any
  // input/textarea/contenteditable to avoid stealing keyboard input.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const meta = e.metaKey || e.ctrlKey;
      if (!meta) return;
      if (e.key.toLowerCase() !== "k") return;
      const target = e.target as HTMLElement | null;
      // Allow opening from focused inputs too — the palette steals
      // focus once open, and Cmd+K is a near-universal "open palette"
      // shortcut. The only reason to suppress would be if a child
      // editor (Monaco etc.) had its own Cmd+K binding, which we
      // don't have today.
      void target;
      e.preventDefault();
      setOpen((v) => {
        // Closing via the toggle must reset like close()/Escape/click-outside,
        // otherwise the stale query + active index persist and the palette
        // reopens mid-filtered with the wrong row highlighted.
        if (v) {
          setQuery("");
          setActiveIdx(0);
        }
        return !v;
      });
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    const onOpen = () => setOpen(true);
    window.addEventListener(OPEN_COMMAND_PALETTE_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_COMMAND_PALETTE_EVENT, onOpen);
  }, []);

  useEffect(() => {
    if (open) {
      const id = window.setTimeout(() => inputRef.current?.focus(), 0);
      return () => window.clearTimeout(id);
    }
  }, [open]);

  // Keep the arrow-key-selected command visible: without this, holding ↓ moves
  // the highlight past the bottom of the scroll viewport and the user can't see
  // what's selected. `block: "nearest"` avoids gratuitous scrolling when it's
  // already in view. Depends on `open` since the ref only mounts while open.
  useEffect(() => {
    if (open) activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIdx, open]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands.filter((c) => c.group !== "Games");
    return commands.filter((c) => {
      if (c.label.toLowerCase().includes(q)) return true;
      if (c.keywords?.some((k) => k.toLowerCase().includes(q))) return true;
      if (c.hint?.toLowerCase().includes(q)) return true;
      return false;
    });
  }, [commands, query]);

  // Group by `group` field, preserving original order within group.
  const grouped = useMemo(() => {
    const out: Array<{ group: string; items: Command[] }> = [];
    for (const c of filtered) {
      const last = out[out.length - 1];
      if (last && last.group === c.group) last.items.push(c);
      else out.push({ group: c.group, items: [c] });
    }
    return out;
  }, [filtered]);

  // Reset active index whenever filter changes.
  useEffect(() => {
    setActiveIdx(0);
  }, [query]);

  if (!open) return null;

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIdx((i) => Math.min(i + 1, filtered.length - 1));
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIdx((i) => Math.max(i - 1, 0));
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const cmd = filtered[activeIdx];
      if (cmd) void cmd.run();
    }
  };

  // Track the running flat index across groups so keyboard navigation
  // matches the visual order.
  let flatIdx = -1;

  return (
    <div
      className="anim-scrim fixed inset-0 z-50 flex items-start justify-center bg-[var(--overlay-scrim)] pt-[15vh]"
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div className="anim-pop glass-float elev-3 w-[560px] max-w-[90vw] overflow-hidden rounded-[var(--radius-panel)]">
        <div className="flex items-center gap-2 border-b border-[var(--color-border)] px-3 py-2">
          <Search size={14} className="text-[var(--color-muted)]" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKey}
            placeholder={tr(
              "cmdpalette_search_placeholder",
              "Type a command or search…",
            )}
            className="flex-1 bg-transparent text-sm outline-none"
          />
          <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs text-[var(--color-muted)]">
            {tr("cmdpalette_esc", "Esc")}
          </span>
        </div>
        <div className="max-h-[50vh] overflow-y-auto py-1">
          {filtered.length === 0 ? (
            <div className="px-4 py-8 text-center text-xs text-[var(--color-muted)]">
              {tr(
                "cmdpalette_no_match",
                { query },
                'No commands match "{query}"',
              )}
            </div>
          ) : (
            grouped.map(({ group, items }) => (
              <div key={group}>
                <div className="px-3 pb-1 pt-2 text-xs font-semibold uppercase tracking-wider text-[var(--color-muted)]">
                  {tr(`cmdpalette_group_${group.toLowerCase()}`, group)}
                </div>
                {items.map((c) => {
                  flatIdx++;
                  const isActive = flatIdx === activeIdx;
                  return (
                    <button
                      key={c.id}
                      ref={isActive ? activeRef : undefined}
                      type="button"
                      onMouseEnter={() => setActiveIdx(flatIdx)}
                      onClick={() => void c.run()}
                      className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm ${
                        isActive
                          ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                          : "hover:bg-[var(--color-surface-3)]"
                      }`}
                    >
                      <span className="min-w-0 flex-1 truncate">{c.label}</span>
                      {c.hint && (
                        <span className="truncate text-xs opacity-60">
                          {c.hint}
                        </span>
                      )}
                      {isActive && <ArrowRight size={12} className="opacity-70" />}
                    </button>
                  );
                })}
              </div>
            ))
          )}
        </div>
        <div className="border-t border-[var(--color-border)] px-3 py-1.5 text-xs text-[var(--color-muted)]">
          {tr(
            "cmdpalette_footer_hint",
            "↑↓ navigate · Enter run · Esc close · Cmd/Ctrl+K toggle",
          )}
        </div>
      </div>
    </div>
  );
}
