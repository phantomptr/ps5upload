// A knock's refusal in words (design 8), apart from knock.ts so the guest page, which shows the
// engine's own refusal words (JoinView endText), doesn't carry these strings too.

import type { KnockNo } from "./knock";

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** A refusal in the room's words (design 8), the same on every surface. A blocked person gets
 *  the plain "said no" (never told they are blocked). */
export function knockNoText(tr: Tr, why: KnockNo, host: string, triesLeft: number | null = null): string {
  switch (why) {
    case "full":
      return tr("room_no_full", undefined, "The room is full.");
    case "locked":
      return tr("room_no_locked", undefined, "The room isn't taking players right now.");
    case "host_away":
      return tr("room_no_host_away", undefined, "Your host isn't there to answer right now.");
    case "password":
      return triesLeft != null
        ? tr("room_no_password_left", { n: triesLeft }, `Wrong password. ${triesLeft} tries left.`)
        : tr("room_no_password", undefined, "Wrong password.");
    case "locked_out":
      return tr("room_no_locked_out", undefined, "Too many tries. Try again in 10 minutes.");
    case "not_listed":
    case "closed":
      return tr("room_no_closed", undefined, "This room is closed.");
    case "not_playing":
      return tr("join_refused_not_playing", { host }, `${host} isn't playing right now. Ask ${host} to start Play first.`);
    case "old":
      return tr("room_no_old", undefined, "Update to join (needs PS5Upload 13.0.0 or newer).");
    case "name_taken":
      return tr("room_no_name_taken", undefined, "Someone in this room already has that name.");
    default:
      return tr("room_no_said_no", { host }, `${host} said no.`);
  }
}
