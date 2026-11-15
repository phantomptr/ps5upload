// Choices under ③ Options that are about the one game checked under ①, and are never
// remembered: they are off again for the next game.

import type { BackportInfo } from "../../api/fpkg";
import { Toggle } from "../../components";
import { useTr } from "../../state/lang";

/** Why the game's backport cannot be left out of what is being made now, or null when it can:
 *  the engine's own reason (the backport changed eboot.bin), or a game in asset packs that
 *  this run does not unpack (`unpacks`: it writes new zstd packs, which decodes the old ones). */
export function backportRefusal(backport: BackportInfo, unpacks: boolean): "blocked" | "packed" | null {
  if (backport.blocked) return "blocked";
  return backport.packed && !unpacks ? "packed" : null;
}

/** "Remove backport libraries": shown for a game whose fakelib holds backport libraries, when
 *  an image or a folder is made of it. The emulators in fakelib are kept, and the source is
 *  not changed; a game the backport changed more deeply says why it stays. */
export function RemoveBackportOption({
  backport,
  unpacks,
  checked,
  onChange,
}: {
  backport: BackportInfo;
  /** This run writes zstd packs, so a packed game is unpacked on the way. */
  unpacks: boolean;
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
  const tr = useTr();
  const refusal = backportRefusal(backport, unpacks);
  const count = backport.libraries.length;
  // The kinds of emulator kept, each once (two AMPR files are one emulator to the reader).
  const kept = [...new Set(backport.emulators.map((e) => e.name))].join(", ");
  return (
    <div className="space-y-1" data-testid="remove-backport-option">
      <Toggle
        checked={checked && !refusal}
        onChange={onChange}
        disabled={!!refusal}
        label={tr("fpkg.backport.remove", undefined, "Remove backport libraries")}
        hint={tr(
          "fpkg.backport.remove_body",
          { count },
          "Leaves the {count} backport libraries in this game's fakelib out of what is made, for a console whose own firmware runs the game. The game you chose is not changed.",
        )}
      />
      {kept && (
        <div className="text-xs text-[var(--color-muted)]" data-testid="remove-backport-kept">
          {tr("fpkg.backport.kept", { names: kept }, "Emulators in fakelib are kept: {names}.")}
        </div>
      )}
      {refusal === "blocked" && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="remove-backport-blocked">
          {tr(
            "fpkg.backport.blocked",
            { reason: backport.blocked ?? "" },
            "Not available for this game: {reason}",
          )}
        </div>
      )}
      {refusal === "packed" && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="remove-backport-packed">
          {tr(
            "fpkg.backport.packed",
            undefined,
            "This game is in asset packs, and their own list names the backport libraries. Turn on zstd asset packs to leave them out: that unpacks the game first.",
          )}
        </div>
      )}
    </div>
  );
}

/** Why the source cannot be moved to the Trash after this conversion, or null when it can. */
export type DeleteSourceRefusal =
  | { kind: "engine"; reason: string }
  | { kind: "archive" | "merge" | "elsewhere" };

/** The first thing in the way of "move the source to the Trash": the engine's own reason (the
 *  source is on a PS5 or a server, or there is no Trash), an archive (what is converted is its
 *  scratch copy), an update merged in (two sources), or an output that is not kept on this
 *  computer (saved straight to the PS5 or a server). The engine checks all of this again, and
 *  that the output is not inside the source, before anything is written. */
export function deleteSourceRefusal(o: {
  engineReason?: string | null;
  archive: boolean;
  merge: boolean;
  elsewhere: boolean;
}): DeleteSourceRefusal | null {
  if (o.engineReason) return { kind: "engine", reason: o.engineReason };
  if (o.archive) return { kind: "archive" };
  if (o.merge) return { kind: "merge" };
  return o.elsewhere ? { kind: "elsewhere" } : null;
}

/** "Move the source to the Trash once the output is verified": for an image or folder made of
 *  a file or folder on this computer. Off unless ticked for this one conversion, and ticking
 *  it asks first, naming the path (the screen does that). */
export function DeleteSourceOption({
  path,
  refusal,
  checked,
  onChange,
}: {
  /** What would go. */
  path: string;
  refusal: DeleteSourceRefusal | null;
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
  const tr = useTr();
  const why =
    refusal?.kind === "engine"
      ? tr("fpkg.delete_source.no_engine", { reason: refusal.reason }, "Not available: {reason}")
      : refusal?.kind === "archive"
        ? tr(
            "fpkg.delete_source.no_archive",
            undefined,
            "Not available for an archive: it is unpacked to a scratch copy first, and the archive itself is kept.",
          )
        : refusal?.kind === "merge"
          ? tr(
              "fpkg.delete_source.no_merge",
              undefined,
              "Not available with an update merged in: there are two sources.",
            )
          : refusal?.kind === "elsewhere"
            ? tr(
                "fpkg.delete_source.no_elsewhere",
                undefined,
                "Not available when the output is saved straight to the PS5 or a server: only for an output kept on this computer.",
              )
            : null;
  const on = checked && !refusal;
  return (
    <div className="space-y-1" data-testid="delete-source-option">
      <Toggle
        checked={on}
        onChange={onChange}
        disabled={!!refusal}
        label={tr("fpkg.delete_source", undefined, "Move the source to the Trash once the output is verified")}
        hint={tr(
          "fpkg.delete_source_body",
          undefined,
          "After the output is written, every byte of it is read back and checked, and only then is the game you chose moved to the Trash or Recycle Bin. It stays if the conversion fails or is stopped, or if it changed meanwhile. For this conversion only: it is off again afterwards, and games in the queue are never removed.",
        )}
      />
      {on && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="delete-source-path">
          {tr("fpkg.delete_source.path", { path }, "Will be moved to the Trash: {path}")}
        </div>
      )}
      {why && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="delete-source-refused">
          {why}
        </div>
      )}
    </div>
  );
}
