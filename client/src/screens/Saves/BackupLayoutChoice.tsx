import { formatDate } from "../../lib/formatDate";
import type {
  BackupLayout,
  DecryptedCopyStatus,
  SaveBackupSettings,
  ServerBackupSettings,
} from "../../api/saveBackup";
import { useTr } from "../../state/lang";

/** The layout settings a card changes (#886). */
export interface LayoutSettings {
  layout?: BackupLayout;
  game_names?: boolean;
  decrypted_copy?: boolean;
}

/** Whether the engine can arrange backups by user: an engine before 12.6.16 leaves `layout`
 *  out of its settings, and keeps every backup by game. */
export function layoutSupported(settings: LayoutSettings | null | undefined): boolean {
  return typeof settings?.layout === "string";
}

/** Where a backup goes in the folder, as an example path (what the engine writes; a user or game
 *  folder already there is used as it is). */
export function layoutExample(layout: BackupLayout, gameNames: boolean): string {
  const file = "PPSA01234_user1a2b3c4d_2026-11-17_101500.zip";
  if (layout === "by_game") return `PPSA01234/${file}`;
  const game = gameNames ? "PPSA01234 - Game Name" : "PPSA01234";
  return `Alex_1a2b3c4d/Encrypted/PS5/${game}/${file}`;
}

/** Where "Save as a file for a friend or another PS5" can put the file: the user's Decrypted
 *  folder of each backup place arranged by user, then this device. Empty when no place is
 *  arranged by user (the file is saved on this device, as before). */
export type ExportDest = "local" | "server" | "device";

export function exportDests(
  local: Pick<SaveBackupSettings, "dir" | "layout"> | null | undefined,
  server: Pick<ServerBackupSettings, "target" | "layout"> | null | undefined,
): ExportDest[] {
  const out: ExportDest[] = [];
  if (local?.layout === "by_user" && local.dir.trim()) out.push("local");
  if (server?.layout === "by_user" && server.target.trim()) out.push("server");
  if (out.length > 0) out.push("device");
  return out;
}

/** "Folder layout: By game / By user" for a backup place (the backup folder, the network share)
 *  and its PS5 drive copy, with the by-user extras: the game's name in its folder, and a decrypted
 *  copy with each backup (which opens on any PS5, so it says so). */
export function BackupLayoutChoice({
  settings,
  status,
  onChange,
  testId,
}: {
  settings: LayoutSettings;
  status: DecryptedCopyStatus | null | undefined;
  onChange: (patch: LayoutSettings) => void;
  testId: string;
}) {
  const tr = useTr();
  const layout = settings.layout ?? "by_game";
  const byUser = layout === "by_user";
  const gameNames = !!settings.game_names;
  return (
    <div className="mt-3 border-t border-[var(--color-border)] pt-3 text-sm" data-testid={testId}>
      <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <label htmlFor={`${testId}-select`} className="text-xs text-[var(--color-muted)]">
          {tr("saves_layout_label", undefined, "Folder layout")}
        </label>
        <select
          id={`${testId}-select`}
          data-testid={`${testId}-select`}
          value={layout}
          onChange={(e) => onChange({ layout: e.target.value as BackupLayout })}
          className="select w-auto! justify-self-start text-sm sm:justify-self-end"
        >
          <option value="by_game">{tr("saves_layout_by_game", undefined, "By game")}</option>
          <option value="by_user">{tr("saves_layout_by_user", undefined, "By user")}</option>
        </select>
      </div>
      <p className="mt-1 break-all font-mono text-[11px] text-[var(--color-muted)]" data-testid={`${testId}-example`}>
        {layoutExample(layout, gameNames)}
      </p>
      <p className="mt-1 text-xs text-[var(--color-muted)]">
        {byUser
          ? tr(
              "saves_layout_by_user_note",
              undefined,
              "Each user gets a folder named after them and their account ID, with Encrypted (backups that restore on this PS5) and Decrypted (files for a friend or another PS5), each split into PS4 and PS5. Backups made before stay where they are and still show.",
            )
          : tr("saves_layout_by_game_note", undefined, "One folder per game, every user's backups in it.")}
      </p>
      {byUser && (
        <>
          <label className="mt-2 flex items-center gap-2">
            <input
              type="checkbox"
              checked={gameNames}
              onChange={(e) => onChange({ game_names: e.target.checked })}
              data-testid={`${testId}-game-names`}
            />
            {tr("saves_layout_game_names", undefined, "Add the game's name to its folder")}
          </label>
          <label className="mt-2 flex items-center gap-2">
            <input
              type="checkbox"
              checked={!!settings.decrypted_copy}
              onChange={(e) => onChange({ decrypted_copy: e.target.checked })}
              data-testid={`${testId}-decrypted`}
            />
            {tr("saves_layout_decrypted_copy", undefined, "Also save a decrypted copy of each backup")}
          </label>
          <p className="mt-1 text-xs text-[var(--color-warn)]" data-testid={`${testId}-decrypted-warning`}>
            {tr(
              "saves_layout_decrypted_warning",
              undefined,
              "Decrypted copies are not locked to this PS5: anyone who has the file can load the save on another PS5. PS5 saves only. To make each one, the save is opened on the PS5 for a moment.",
            )}
          </p>
          {status && status.at > 0 && (
            <p className="mt-1 text-xs text-[var(--color-muted)]" data-testid={`${testId}-decrypted-status`}>
              {tr(
                "saves_layout_decrypted_last",
                { when: formatDate(new Date(status.at)), ok: status.made, failed: status.failed },
                "Last decrypted copies ({when}): {ok} made, {failed} failed.",
              )}
              {status.note ? ` ${status.note}` : ""}
            </p>
          )}
        </>
      )}
    </div>
  );
}
