// Names in words: what to fix in one, and the default room name. Apart from names.ts so the guest
// page (which bundles names.ts for its link check) doesn't carry these strings.

import { cleanName, type NameProblem } from "./names";

/** The default room name: "<host>'s room" (the words are the app's). */
export function defaultRoomName(tr: (key: string, vars: Record<string, string>, fallback: string) => string, host: string): string {
  const who = cleanName(host).slice(0, 20) || "PS5";
  return tr("room_default_name", { name: who }, `${who}'s room`);
}

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** What to fix in a name, in words. */
export function nameProblemText(tr: Tr, p: NameProblem, room: boolean): string {
  switch (p) {
    case "length":
      return room
        ? tr("room_name_length", undefined, "Use 3 to 32 characters.")
        : tr("room_player_name_length", undefined, "Use 1 to 24 characters.");
    case "chars":
      return tr("room_name_chars", undefined, "Use letters, numbers, spaces and simple punctuation.");
    case "link":
      return tr("room_name_link", undefined, "Names can't have links.");
    case "code":
      return tr("room_name_code", undefined, "Names can't have codes or long numbers.");
    case "reserved":
      return tr("room_name_reserved", undefined, "Pick another name.");
    case "spam":
      return tr("room_name_spam", undefined, "Pick a name with fewer repeats and symbols.");
  }
}
