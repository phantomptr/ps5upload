// The engine's refusal of a save that needs slots the target doesn't have (save_copy.rs), said
// plainly. Some games give every save a name of its own (The Witcher 3:
// `Manual.53db9.7ea54c00.4f3261a`), so another user never makes a slot of the same name and
// "save in the same slot" can't help them.
//
// - "file": a save file (decrypted files) only goes into a slot the user has. The way round it is
//   a copy on the PS5 the save is on, which adds the slots.
// - "copy": a copy or a backup adds the missing slots whole and makes them theirs; it is refused
//   only when the helper on the PS5 is too old to change a save's owner.
import type { Translator } from "../../state/lang";
import { partLabel } from "../../api/saveShare";

export type MissingFrom = "file" | "copy";

export function partMissingText(
  tr: Translator,
  missing: readonly string[],
  game: string,
  user: string,
  from: MissingFrom,
): string {
  const main = tr("saves_part_main", undefined, "main save");
  const part = missing.map((m) => partLabel(m, main)).join(", ");
  return from === "file"
    ? tr(
        "saves_part_missing_file",
        { game, part, user },
        "{user} doesn't have {game}'s {part} yet, and a save file can only go into a save slot {user} already has. Start the game as {user}, save in the same slot, then try again. If the game gives every save a name of its own (The Witcher 3 does), use Copy to another user or Copy to another console on the PS5 the save is on instead.",
      )
    : tr(
        "saves_part_missing_helper",
        { game, part, user },
        "{user} doesn't have {game}'s {part} yet, and the helper running on the PS5 is too old to add save slots for {user}. Send the helper from this version of the app (Connection), then try again.",
      );
}
