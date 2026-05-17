import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ArrowRight, Search } from "lucide-react";

import { useThemeStore } from "../state/theme";
import { useConnectionStore } from "../state/connection";
import { pushNotification } from "../state/notifications";
import { useTr } from "../state/lang";

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

interface Command {
  id: string;
  label: string;
  keywords?: string[];
  /** Optional secondary line for context. */
  hint?: string;
  /** Group label for visual sectioning. */
  group: "Navigation" | "Theme" | "Connection" | "Window";
  run: () => void | Promise<void>;
}

function useCommands(close: () => void): Command[] {
  const navigate = useNavigate();
  const toggleTheme = useThemeStore((s) => s.toggleTheme);
  const setHost = useConnectionStore((s) => s.setHost);

  return useMemo<Command[]>(() => {
    const nav = (to: string, label: string, keywords?: string[]): Command => ({
      id: `nav:${to}`,
      label,
      keywords,
      group: "Navigation",
      hint: to,
      run: () => {
        navigate(to);
        close();
      },
    });

    return [
      nav("/dashboard", "Dashboard", ["home", "overview"]),
      nav("/connection", "Connection", ["host", "ip", "connect"]),
      nav("/upload", "Upload"),
      nav("/install-package", "Install Package", ["pkg"]),
      nav("/library", "Library", ["games", "apps"]),
      nav("/saves", "Save data"),
      nav("/screenshots", "Screenshots"),
      nav("/search", "Search"),
      nav("/volumes", "Volumes", ["disk", "drives"]),
      nav("/disk-usage", "Disk usage"),
      nav("/file-system", "File System", ["browse", "files"]),
      nav("/hardware", "Hardware", ["temps", "power"]),
      // 2.12.0 merged the SendPayload + KernelLog screens into tabs.
      // Keep old labels as keyword aliases for muscle memory; the
      // canonical entries are now /payloads and /logs with tabs.
      nav("/payloads", "Payloads", [
        "homebrew catalog",
        "payload library",
        "kstuff",
        "shadowmount",
        "etahen",
      ]),
      nav("/payloads?tab=send", "Send file (Payloads)", ["send payload"]),
      nav("/activity", "Activity"),
      nav("/stats", "Stats"),
      nav("/logs", "Logs"),
      nav("/logs?tab=kernel", "Kernel log (Logs)", ["dmesg", "klog"]),
      nav("/shell", "Shell", ["terminal"]),
      nav("/settings", "Settings"),
      nav("/about", "About"),
      nav("/faq", "FAQ", ["help"]),
      {
        id: "theme:toggle",
        label: "Toggle theme",
        keywords: ["dark", "light", "oled"],
        group: "Theme",
        run: () => {
          toggleTheme();
          close();
        },
      },
      {
        id: "conn:paste-host",
        label: "Paste host from clipboard",
        keywords: ["ip", "address", "connect"],
        group: "Connection",
        hint: "Reads navigator.clipboard for an IPv4 address",
        run: async () => {
          try {
            const text = await navigator.clipboard.readText();
            const match = text.match(/(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})/);
            if (match) {
              setHost(match[1]);
              pushNotification("info", "Host updated", {
                body: `Set to ${match[1]} from clipboard.`,
              });
            } else {
              pushNotification("warning", "No IP found", {
                body: "Clipboard didn't contain an IPv4 address.",
              });
            }
          } catch {
            pushNotification("warning", "Clipboard read failed", {
              body: "Permission denied or unavailable.",
            });
          }
          close();
        },
      },
    ];
  }, [navigate, toggleTheme, setHost, close]);
}

export function CommandPalette() {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIdx, setActiveIdx] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

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
      setOpen((v) => !v);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    if (open) {
      const id = window.setTimeout(() => inputRef.current?.focus(), 0);
      return () => window.clearTimeout(id);
    }
  }, [open]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
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
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 pt-[15vh]"
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div className="w-[560px] max-w-[90vw] overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] shadow-xl">
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
          <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-[10px] text-[var(--color-muted)]">
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
                <div className="px-3 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wider text-[var(--color-muted)]">
                  {group}
                </div>
                {items.map((c) => {
                  flatIdx++;
                  const isActive = flatIdx === activeIdx;
                  return (
                    <button
                      key={c.id}
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
                        <span className="truncate text-[10px] opacity-60">
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
        <div className="border-t border-[var(--color-border)] px-3 py-1.5 text-[10px] text-[var(--color-muted)]">
          {tr(
            "cmdpalette_footer_hint",
            "↑↓ navigate · Enter run · Esc close · Cmd/Ctrl+K toggle",
          )}
        </div>
      </div>
    </div>
  );
}
