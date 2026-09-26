import { useState } from "react";
import { useStore } from "zustand";
import { Plus, X, Check } from "lucide-react";

import { useRosterStore, profileAccentForHost } from "../state/roster";
import { useHostRuntime } from "../state/connection";
import { useUploadQueueStore } from "../state/uploadQueue";
import { pkgLibraryStore } from "../state/pkgLibrary";
import { hostOf } from "../lib/addr";
import { useTr } from "../state/lang";

/**
 * Console tab strip — one tab per PS5, always visible above the content.
 *
 * Each tab is effectively its own ps5upload "instance": switching tabs changes
 * which console you VIEW, but every console's operations (uploads + installs)
 * keep running in their own per-host background loops regardless of which tab
 * is active. Each tab shows a live status dot (from the fan-out poller's
 * per-host runtime) and an activity badge when that console is uploading or
 * installing — so you can run up to ~12 consoles at once and see them all.
 */

function StatusDot({ host }: { host: string }) {
  const rt = useHostRuntime(host);
  const color =
    rt.payloadStatus === "up"
      ? "var(--color-good)"
      : rt.payloadStatus === "down"
        ? "var(--color-bad)"
        : "var(--color-muted)";
  return (
    <span
      className="inline-block h-2 w-2 shrink-0 rounded-full"
      style={{ background: color }}
      aria-hidden
    />
  );
}

/** True when this console has an upload or install running right now. */
function useConsoleBusy(host: string): boolean {
  const key = hostOf(host);
  const uploading = useUploadQueueStore((s) => !!s.runningHosts[key]);
  const installing = useStore(pkgLibraryStore(host), (s) => s.installing);
  return uploading || installing;
}

function ConsoleTab({
  id,
  name,
  host,
  accent,
  active,
  onClick,
}: {
  id: string;
  name: string;
  host: string;
  /** This console's identity color (roster-position palette). */
  accent: string | null;
  active: boolean;
  onClick: () => void;
}) {
  const busy = useConsoleBusy(host);
  return (
    <button
      type="button"
      onClick={onClick}
      title={`${name} — ${host}`}
      aria-current={active ? "page" : undefined}
      data-console-id={id}
      className={`flex max-w-[14rem] shrink-0 items-center gap-2 rounded-t-md border-x border-t-2 px-3 py-1.5 text-sm ${
        active
          ? "border-x-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-text)]"
          : "border-x-transparent text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
      }`}
      // Identity stripe: each console keeps its own color on the tab's top
      // edge (full strength when active, dimmed otherwise). Two same-model
      // consoles with similar default names stay tellable apart at a
      // glance, and the same color repeats on activity chips so rows match
      // tabs without reading names. Hex palette → append alpha for the dim.
      style={{
        borderTopColor: accent
          ? active
            ? accent
            : `${accent}55`
          : "transparent",
      }}
    >
      <StatusDot host={host} />
      <span className="truncate">{name}</span>
      {busy && (
        <span
          className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full"
          style={{ background: accent ?? "var(--color-accent)" }}
          aria-hidden
        />
      )}
    </button>
  );
}

export default function ConsoleTabs() {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const activeId = useRosterStore((s) => s.active_id);
  const setActive = useRosterStore((s) => s.setActive);
  const add = useRosterStore((s) => s.add);

  const [adding, setAdding] = useState(false);
  const [ip, setIp] = useState("");
  const [name, setName] = useState("");

  // Below ~2 consoles the strip adds clutter without value — only show it once
  // the user actually manages multiple PS5s (single-console users keep the
  // clean layout). The sidebar roster picker remains for full management.
  if (profiles.length < 2 && !adding) return null;

  const submitAdd = () => {
    const host = ip.trim();
    if (!host) {
      setAdding(false);
      return;
    }
    const idNew = add({ name: name.trim() || `PS5 (${host})`, host });
    setActive(idNew);
    setIp("");
    setName("");
    setAdding(false);
  };

  return (
    <div className="flex items-end gap-1 overflow-x-auto border-b border-[var(--color-border)] bg-[var(--color-surface)] px-2 pt-1.5">
      {profiles.map((p) => (
        <ConsoleTab
          key={p.id}
          id={p.id}
          name={p.name}
          host={p.host}
          accent={profileAccentForHost(p.host, profiles)}
          active={p.id === activeId}
          onClick={() => setActive(p.id)}
        />
      ))}

      {adding ? (
        <div className="mb-1 flex shrink-0 items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-1.5 py-1">
          <input
            autoFocus
            value={ip}
            onChange={(e) => setIp(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submitAdd();
              if (e.key === "Escape") setAdding(false);
            }}
            placeholder={tr("console_tab_ip", "PS5 IP")}
            inputMode="decimal"
            className="w-28 rounded bg-[var(--color-surface)] px-2 py-1 text-sm outline-none"
          />
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submitAdd();
              if (e.key === "Escape") setAdding(false);
            }}
            placeholder={tr("console_tab_name", "Name (optional)")}
            className="w-32 rounded bg-[var(--color-surface)] px-2 py-1 text-sm outline-none"
          />
          <button
            type="button"
            onClick={submitAdd}
            aria-label={tr("add", "Add")}
            className="rounded p-1 text-[var(--color-good)] hover:bg-[var(--color-surface-3)]"
          >
            <Check size={16} />
          </button>
          <button
            type="button"
            onClick={() => setAdding(false)}
            aria-label={tr("cancel", "Cancel")}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
          >
            <X size={16} />
          </button>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          aria-label={tr("console_tab_add", "Add a console")}
          title={tr("console_tab_add", "Add a console")}
          className="mb-1 shrink-0 rounded-md px-2 py-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          <Plus size={16} />
        </button>
      )}
    </div>
  );
}
