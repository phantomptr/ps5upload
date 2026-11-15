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
