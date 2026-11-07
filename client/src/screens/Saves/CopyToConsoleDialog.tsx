// "Copy to › Another console…" (#709): pick the console, then the user or the folder there. The
// walk through a copy is Copy to another user's; only where it goes differs.
import { useState } from "react";
import { MonitorSmartphone } from "lucide-react";

import { Button, Modal, Spinner } from "../../components";
import type { ProfileUser, SaveEntry } from "../../api/ps5";
import { copyConsoleTargets, copySaveToConsole } from "../../api/saveShare";
import { humanizeCaught } from "../../lib/humanizeError";
import { useSaveGameName } from "./gameNames";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import CopyToUserDialog from "./CopyToUserDialog";
import { copyTargets, type CopyTarget } from "./copyTargets";

/** An emulator's save: plain files, in a folder rather than a user's own save. */
export function isEmulatorSave(e: SaveEntry): boolean {
  return e.kind !== "ps5" && e.kind !== "ps4";
}

/** The folder's name, which is all an emulator's profile has here. */
function placeName(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** Where a save can go on the other console. A PS5 save goes into a user's own save of the game,
 *  so one with none can't be picked. An emulator's goes into the same kind of folder, or, with none
 *  there yet, the same place it has here. */
export function consoleTargets(
  entry: SaveEntry,
  saves: readonly SaveEntry[],
  users: readonly ProfileUser[],
  samePlace: string,
): CopyTarget[] {
  if (!isEmulatorSave(entry)) return copyTargets(users, saves, entry.title_id, 0);
  const same = saves.filter((s) => s.kind === entry.kind);
  if (same.length === 0) return [{ uid: entry.user_id, name: samePlace, save: entry }];
  return same.map((s) => ({ uid: s.user_id, name: placeName(s.path), save: s }));
}

export function CopyToConsoleDialog({
  entry,
  owner,
  fromHost,
  titleId,
  onClose,
}: {
  entry: SaveEntry;
  owner: string;
  fromHost: string;
  titleId: string;
  onClose: () => void;
}) {
  const tr = useTr();
  const game = useSaveGameName(titleId).name ?? titleId;
  const profiles = useRosterStore((r) => r.profiles);
  const others = profiles.filter((p) => p.host !== fromHost);
  const [to, setTo] = useState<{ host: string; name: string } | null>(null);
  const [targets, setTargets] = useState<CopyTarget[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const pick = async (host: string, name: string) => {
    setTo({ host, name });
    setError(null);
    try {
      const there = await copyConsoleTargets(host, entry.title_id);
      setTargets(
        consoleTargets(
          entry,
          there.saves,
          there.users,
          tr("saves_copy_same_place", undefined, "The same place on that console"),
        ),
      );
    } catch (e) {
      setTo(null);
      setError(humanizeCaught(e));
    }
  };

  const title = to
    ? tr("saves_copy_console_title_to", { console: to.name }, "Copy to {console}")
    : tr("saves_copy_console_title", undefined, "Copy to another console");

  if (to && targets) {
    return (
      <CopyToUserDialog
        title={title}
        game={game}
        source={{ kind: "save", owner }}
        targets={targets}
        run={async (t) => {
          await copySaveToConsole({
            from: fromHost,
            to: to.host,
            from_path: entry.path,
            to_user: t.uid,
            to_path: isEmulatorSave(entry) ? t.save?.path : undefined,
          });
        }}
        onClose={onClose}
      />
    );
  }
  return (
    <Modal open onClose={onClose} title={title} size="md" bodyClassName="p-4 sm:p-5">
      <div className="flex flex-col gap-3" data-testid="copy-console-picker">
        {to ? (
          <p className="flex items-center gap-2 text-sm">
            <Spinner size={14} />
            {tr("saves_copy_console_asking", { console: to.name }, "Asking {console} what it has…")}
          </p>
        ) : (
          <>
            <p className="text-sm">
              {tr("saves_copy_console_pick", { source: owner, game }, "Copy {source}'s save of {game} to:")}
            </p>
            <ul className="flex flex-col gap-1">
              {others.map((p) => (
                <li key={p.id}>
                  <button
                    type="button"
                    data-testid="copy-console-target"
                    onClick={() => void pick(p.host, p.name)}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-start text-sm hover:bg-[var(--color-surface-raised)]"
                  >
                    <MonitorSmartphone size={14} className="shrink-0" aria-hidden />
                    <span className="min-w-0 flex-1 truncate font-medium">{p.name}</span>
                    <span className="text-xs text-[var(--color-muted)]">{p.host}</span>
                  </button>
                </li>
              ))}
            </ul>
          </>
        )}
        {error && <p className="text-xs text-[var(--color-bad)]">{error}</p>}
        <div className="flex justify-end">
          <Button variant="ghost" size="sm" onClick={onClose}>
            {tr("saves_copy_cancel", undefined, "Cancel")}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
