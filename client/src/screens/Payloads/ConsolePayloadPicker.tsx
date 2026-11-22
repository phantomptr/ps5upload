// "Add from the PS5…" (#900): pick a .elf/.bin stored on the console for a playlist step. The
// step keeps only the path; the payload is read from whichever console the playlist runs on.

import { useEffect, useState } from "react";
import { ArrowUp, Cpu, Folder } from "lucide-react";

import { fsListDir } from "../../api/ps5";
import { Button, Modal } from "../../components";
import { consoleParentDir } from "../../lib/consolePath";
import { humanizeCaught } from "../../lib/humanizeError";
import { isConsolePayloadName } from "../../lib/playlistOps";
import { useTr } from "../../state/lang";

const START_DIR = "/data";

const join = (dir: string, name: string) => (dir === "/" ? `/${name}` : `${dir}/${name}`);

export function ConsolePayloadPicker({
  open,
  host,
  onClose,
  onPick,
}: {
  open: boolean;
  host: string;
  onClose: () => void;
  onPick: (path: string) => void;
}) {
  const tr = useTr();
  const [dir, setDir] = useState(START_DIR);
  const [entries, setEntries] = useState<{ dirs: string[]; files: string[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [chosen, setChosen] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !host.trim()) return;
    let cancelled = false;
    setEntries(null);
    setError(null);
    setChosen(null);
    fsListDir(host, dir)
      .then((list) => {
        if (cancelled) return;
        const byName = (a: string, b: string) => a.localeCompare(b);
        setEntries({
          dirs: list.filter((e) => e.kind === "dir").map((e) => e.name).sort(byName),
          files: list
            .filter((e) => e.kind === "file" && isConsolePayloadName(e.name))
            .map((e) => e.name)
            .sort(byName),
        });
      })
      .catch((e) => {
        if (cancelled) return;
        setEntries({ dirs: [], files: [] });
        setError(humanizeCaught(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, host, dir]);

  const row =
    "flex w-full items-center gap-2 px-2 py-1.5 text-start text-xs hover:bg-[var(--color-surface-3)]";
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="md"
      title={tr("playlist_ps5_picker_title", undefined, "Choose a payload on the PS5")}
      footer={
        <>
          <Button variant="ghost" size="sm" onClick={onClose}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={!chosen}
            onClick={() => {
              if (chosen) onPick(chosen);
            }}
          >
            {tr("playlist_ps5_picker_add", undefined, "Add step")}
          </Button>
        </>
      }
    >
      <div className="grid gap-2 p-5">
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "playlist_ps5_picker_hint",
            undefined,
            "The step sends this file from the PS5 the playlist runs on, so it works from any device.",
          )}
        </p>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => setDir(consoleParentDir(dir))}
            disabled={dir === "/"}
            aria-label={tr("fs_up", undefined, "Up")}
            className="rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-1 hover:bg-[var(--color-float)] disabled:opacity-40"
          >
            <ArrowUp size={14} />
          </button>
          <span className="min-w-0 flex-1 truncate font-mono text-xs" title={dir}>
            {dir}
          </span>
        </div>
        <div className="max-h-72 overflow-y-auto rounded-[var(--radius-card)] border border-[var(--glass-edge)]">
          {entries === null ? (
            <div className="p-2 text-xs text-[var(--color-muted)]">{tr("loading", undefined, "Loading…")}</div>
          ) : error ? (
            <div className="p-2 text-xs text-[var(--color-bad)]">{error}</div>
          ) : entries.dirs.length + entries.files.length === 0 ? (
            <div className="p-2 text-xs text-[var(--color-muted)]">
              {tr("playlist_ps5_picker_empty", undefined, "No folders or payloads (.elf, .bin) here.")}
            </div>
          ) : (
            <>
              {entries.dirs.map((name) => (
                <button key={`d:${name}`} type="button" className={row} onClick={() => setDir(join(dir, name))}>
                  <Folder size={14} className="shrink-0 text-[var(--color-muted)]" />
                  <span className="truncate">{name}</span>
                </button>
              ))}
              {entries.files.map((name) => {
                const path = join(dir, name);
                return (
                  <button
                    key={`f:${name}`}
                    type="button"
                    aria-pressed={chosen === path}
                    className={`${row} ${chosen === path ? "bg-[var(--color-surface-3)] font-semibold" : ""}`}
                    onClick={() => setChosen(path)}
                    onDoubleClick={() => onPick(path)}
                  >
                    <Cpu size={14} className="shrink-0 text-[var(--color-accent)]" />
                    <span className="truncate font-mono">{name}</span>
                  </button>
                );
              })}
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
