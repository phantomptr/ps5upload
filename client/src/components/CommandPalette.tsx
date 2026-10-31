import { useCollectionStore } from "../state/collection";
import { gamePath } from "../lib/gamePage";
import { gamesApi, type InstalledGame } from "../api/games";
import { checkForUpdatesNow } from "../layout/UpdateCheckButton";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ArrowRight, Search } from "lucide-react";

import { useThemeStore } from "../state/theme";
import { profileNameForHost, selectConsoleByAddress, useRosterStore } from "../state/roster";
import { useConnectionStore } from "../state/connection";
import { hostOf } from "../lib/addr";
import { pushNotification } from "../state/notifications";
import { activateFocusTrap } from "../lib/focusTrap";
import { useTr, type Translator } from "../state/lang";
import { useBetaFeaturesStore } from "../state/betaFeatures";
import { isTauriEnv } from "../lib/tauriEnv";
import {
  FIXED_NAV_ITEMS,
  isPinnableNav,
  MAX_PINNED,
  NAV_ITEMS,
  navItemOffered,
  navItemVisible,
  pinnedFirst,
  type NavItem,
} from "../layout/navItems";
import { useNavSidebarStore } from "../state/navSidebar";
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
  "/send": ["send to ps5", "send", "upload", "install package", "pkg", "install", "link", "url", "usb", "stream", "archive"],
  "/games": ["library", "apps", "installed", "dump", "dumper", "app dumper"],
  "/workshop": ["convert", "convert games", "game images", "edit image", "look inside", "fpkg", "pkg", "exfat", "ffpfsc", "lz4", "compress"],
  "/shared-folders": ["network shares", "mesh drive", "nas", "smb", "ftp", "sftp", "server", "sync"],
  "/accounts": ["profile", "users", "avatar", "username", "account id", "remote play", "pin", "pair", "sign in"],
  "/captures": ["screenshots", "videos", "clips"],
  "/volumes": ["disk", "drives"],
  "/files": ["browse", "file system"],
  "/console": ["hardware", "temps", "power"],
  "/payloads": [
    "homebrew catalog",
    "payload library",
    "kstuff",
    "shadowmount",
    "etahen",
    "nanodns",
    "hearbridge",
    "hush",
    "payload manager",
    "processes",
    "ports",
    "task manager",
    "kill",
  ],
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
  group: "Navigation" | "Theme" | "Connection" | "Window" | "Games" | "Play" | "Pins";
  run: () => void | Promise<void>;
}

/**
 * The palette's screens: Home, Play and Bug report, then the user's pinned screens in pin order,
 * then the rest in sidebar order (same visibility rules as the sidebar).
 */
export function paletteScreens(
  pinned: readonly string[],
  where: { betaEnabled: boolean; inBrowser: boolean; macOrLinux: boolean },
): NavItem[] {
  const shown = (i: NavItem) =>
    navItemOffered(i, { inBrowser: where.inBrowser, macOrLinux: where.macOrLinux }) &&
    navItemVisible(i, where.betaEnabled);
  return [...FIXED_NAV_ITEMS, ...pinnedFirst(NAV_ITEMS, pinned)].filter(shown);
}

/**
 * The palette's games: the Collection's, then what the saved consoles were last read to have
 * installed that the Collection lacks. One entry per title ID; the hint says where it is.
 */
export function paletteGames(
  collection: readonly { game_id: string; title: string }[],
  installed: readonly InstalledGame[],
  hostLabel: (host: string) => string,
  tr: Translator,
): Array<{ id: string; title: string; hint: string }> {
  const onConsoles = new Map(installed.map((g) => [g.title_id.toUpperCase(), g]));
  const where = (id: string, fallback: string) => {
    const g = onConsoles.get(id.toUpperCase());
    if (!g || g.hosts.length === 0) return fallback;
    const names = g.hosts.map(hostLabel).join(", ");
    return tr("cmdpalette_game_installed_on", { consoles: names }, "Installed on {consoles}");
  };
  const out = collection.map((g) => ({ id: g.game_id, title: g.title, hint: where(g.game_id, g.game_id) }));
  const seen = new Set(collection.map((g) => g.game_id.toUpperCase()));
  for (const g of installed) {
    if (seen.has(g.title_id.toUpperCase())) continue;
    out.push({ id: g.title_id, title: g.title || g.title_id, hint: where(g.title_id, g.title_id) });
  }
  return out;
}

function useCommands(close: () => void, open: boolean): Command[] {
  const navigate = useNavigate();
  const toggleTheme = useThemeStore((s) => s.toggleTheme);
  const tr = useTr();
  const library = useCollectionStore((s) => s.library);
  const profiles = useRosterStore((s) => s.profiles);
  const [installed, setInstalled] = useState<InstalledGame[]>([]);
  // Read on each open: what the engine last saw on every saved console, so a game that is only
  // installed (never in the Collection) is found too. Nothing is asked of a console.
  useEffect(() => {
    if (!open) return;
    let live = true;
    gamesApi
      .installed()
      .then((list) => {
        if (live) setInstalled(list);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [open]);
  const games = useMemo(
    () =>
      paletteGames(
        library?.games ? Object.values(library.games) : [],
        installed,
        (h) => profileNameForHost(h, profiles),
        tr,
      ),
    [library, installed, profiles, tr],
  );
  const pinned = useNavSidebarStore((s) => s.pinned);
  const togglePinned = useNavSidebarStore((s) => s.togglePinned);
  // Play (10.0.0): "Play <game>" for what the selected console has installed, once it is paired.
  const host = useConnectionStore((s) => s.host)?.trim() ?? "";
  const paired = profiles.some((p) => p.host === host && !!p.wake_regist_key && !!p.wake_rp_key);
  const playable = useMemo(
    () =>
      paired && host
        ? installed.filter((g) => g.hosts.some((h) => hostOf(h) === hostOf(host)))
        : [],
    [installed, host, paired],
  );

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
    // Pinned screens come first.
    const screens = paletteScreens(pinned, {
      betaEnabled: useBetaFeaturesStore.getState().enabled,
      inBrowser: !isTauriEnv(),
      macOrLinux: isMacOrLinuxDesktop(),
    });
    // "Pin <screen>" / "Unpin <screen>": listed only when searching, like games. Only the pins
    // shown here count toward the limit (a beta screen's pin with beta off doesn't).
    const shownPins = screens.filter((i) => pinned.includes(i.to)).map((i) => i.to);
    const pinCommands = screens
      .filter((i) => isPinnableNav(i.to) && (pinned.includes(i.to) || shownPins.length < MAX_PINNED))
      .map((i): Command => {
        const name = tr(i.key, undefined, i.fallback);
        const isPinned = pinned.includes(i.to);
        return {
          id: `pin:${i.to}`,
          label: isPinned
            ? tr("nav_unpin_item", { name }, "Unpin {name}")
            : tr("nav_pin_item", { name }, "Pin {name}"),
          keywords: ["pin", i.fallback, ...(NAV_KEYWORDS[i.to] ?? [])],
          group: "Pins",
          run: () => {
            togglePinned(i.to, shownPins);
            close();
          },
        };
      });
    return [
      ...screens.map((i) => nav(i.to, i.key, i.fallback, NAV_KEYWORDS[i.to])),
      // Tabs worth jumping to directly.
      // Send is desktop-only (the browser's Payloads screen has no Send tab).
      ...(isTauriEnv()
        ? [nav("/payloads?tab=send", "payloads_tab_send", "Send payload", ["send payload"])]
        : []),
      // Logs and Audit log are tabs of Activity & logs; About and What's new tabs of Help.
      nav("/tasks?tab=logs", "logs", "Logs", ["errors", "app log"]),
      nav("/tasks?tab=logs&log=kernel", "logs_tab_kernel", "Kernel log", ["dmesg", "klog"]),
      nav("/tasks?tab=audit", "audit_log_title", "Audit log", ["deleted", "power actions"]),
      nav("/faq?tab=about", "about", "About", ["version", "license", "credits"]),
      nav("/faq?tab=whats-new", "whats_new", "What's new", ["changelog", "release notes"]),
      // Desktop and mobile only: the web page updates with the engine that serves it.
      ...(isTauriEnv()
        ? [
            {
              id: "update:check",
              label: tr("update_check_button", undefined, "Check for updates"),
              keywords: ["update", "upgrade", "new version", "release"],
              group: "Window" as const,
              run: () => {
                close();
                void checkForUpdatesNow(tr, () => navigate("/settings"));
              },
            },
          ]
        : []),
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
      ...pinCommands,
      // The Collection's games, each to its page. Listed only when searching.
      ...games.map(
        (g): Command => ({
          id: `game:${g.id}`,
          label: g.title,
          keywords: [g.id],
          group: "Games",
          hint: g.hint,
          run: () => {
            navigate(gamePath(g.id));
            close();
          },
        }),
      ),
      // Play a game on the selected console, streamed here. Listed only when searching.
      ...playable.map(
        (g): Command => ({
          id: `play:${g.title_id}`,
          label: tr("cmdpalette_play_game", { title: g.title || g.title_id }, `Play ${g.title || g.title_id}`),
          keywords: [g.title_id, "play", "stream"],
          group: "Play",
          hint: g.title_id,
          run: () => {
            navigate(`/play?launch=${encodeURIComponent(g.title_id)}&name=${encodeURIComponent(g.title || g.title_id)}`);
            close();
          },
        }),
      ),
    ];
  }, [navigate, toggleTheme, close, tr, games, pinned, togglePinned, playable]);
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
  const panelRef = useRef<HTMLDivElement | null>(null);

  const close = () => {
    setOpen(false);
    setQuery("");
    setActiveIdx(0);
  };

  const commands = useCommands(close, open);

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

  // While open: focus the search field, keep Tab inside the panel, and on
  // close hand focus back to whatever had it (the palette used to drop it on
  // <body>, so a keyboard user restarted from the top of the page).
  useEffect(() => {
    if (!open) return;
    const active = document.activeElement;
    const previous = active instanceof HTMLElement ? active : null;
    const id = window.setTimeout(() => inputRef.current?.focus(), 0);
    // Escape rides the shared modal stack like every other modal surface.
    const releaseTrap = activateFocusTrap(() => panelRef.current, window, {
      onEscape: (e) => {
        e.preventDefault();
        setOpen(false);
        setQuery("");
        setActiveIdx(0);
      },
    });
    return () => {
      window.clearTimeout(id);
      releaseTrap();
      // Deferred so the panel has unmounted. Skipped when a command already
      // moved focus somewhere deliberate, or when the opener is gone (a
      // command that navigated away).
      window.setTimeout(() => {
        const now = document.activeElement;
        if (now && now !== document.body) return;
        if (previous?.isConnected) previous.focus({ preventScroll: true });
      }, 0);
    };
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
    if (!q) return commands.filter((c) => c.group !== "Games" && c.group !== "Play" && c.group !== "Pins");
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
      className="anim-scrim scrim fixed inset-0 z-50 flex items-start justify-center pt-[15vh]"
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={tr("cmdpalette_title", "Command palette")}
        tabIndex={-1}
        // On the panel, not the input: the arrow keys and Enter have to work
        // wherever focus is inside the palette (a clicked row, the panel
        // itself), not only while the caret is in the search field.
        onKeyDown={handleKey}
        className="anim-pop glass-float elev-3 w-[560px] max-w-[90vw] overflow-hidden rounded-[var(--radius-panel)] outline-none"
      >
        <div className="field-shell flex items-center gap-3 border-b border-[var(--color-border)] px-5 py-3.5">
          <Search size={17} className="text-[var(--color-muted)]" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={tr(
              "cmdpalette_search_placeholder",
              "Type a command or search…",
            )}
            className="field-quiet flex-1 bg-transparent text-sm"
          />
          <span className="rounded-full border border-[var(--color-border)] px-2 py-0.5 text-xs text-[var(--color-muted)]">
            {tr("cmdpalette_esc", "Esc")}
          </span>
        </div>
        <div className="max-h-[50vh] overflow-y-auto p-2">
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
                <div className="px-4 pb-1 pt-2 text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
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
                      // The list is driven with the arrow keys from the search
                      // field; as Tab stops the rows were 40-odd extra presses.
                      tabIndex={-1}
                      onMouseEnter={() => setActiveIdx(flatIdx)}
                      onClick={() => void c.run()}
                      className={`flex w-full items-center gap-2 rounded-full px-4 py-2 text-left text-sm ${
                        isActive
                          ? "bg-[var(--color-accent-soft)] font-medium text-[var(--color-text)]"
                          : "hover:bg-[var(--color-surface-3)]"
                      }`}
                    >
                      <span className="min-w-0 flex-1 truncate">{c.label}</span>
                      {c.hint && (
                        <span className="truncate text-xs opacity-60">
                          {c.hint}
                        </span>
                      )}
                      {isActive && <ArrowRight size={12} className="opacity-70 rtl:rotate-180" />}
                    </button>
                  );
                })}
              </div>
            ))
          )}
        </div>
        <div className="border-t border-[var(--color-border)] px-5 py-2 text-xs text-[var(--color-muted)]">
          {tr(
            "cmdpalette_footer_hint",
            "↑↓ navigate · Enter run · Esc close · Cmd/Ctrl+K toggle",
          )}
        </div>
      </div>
    </div>
  );
}
