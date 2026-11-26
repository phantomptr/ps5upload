// What an upload's done card says when adding the game folder to the home screen afterwards did
// not work (the queue, uploadQueue.ts, and a single upload, transfer.ts).
//
// "You can still do it from the Library" is said only where that is true. The helper refuses some
// title IDs whoever asks (install_guard.h: one of the PS5's own apps, an ID that is not one, a
// title the console itself installed, #884), and the Library's register asks the same helper:
// for those the reason alone is said.

import { installRefusal, registerRefusedText } from "./consoleInstall";
import { humanizePs5Error } from "./humanizeError";
import { trStatic } from "./trStatic";

/** The Library's register would be refused for the same reason: nothing to point to there. */
export function libraryRefusesToo(raw: string): boolean {
  return /register_system_title|register_title_id_unsafe/.test(raw) || installRefusal(raw)?.op === "register";
}

/** The warning for a register after an upload that failed with `raw` (the helper's or engine's
 *  words). `name`: the game's, when known. */
export function registerAfterUploadWarning(raw: string, name?: string): string {
  const installed = installRefusal(raw);
  if (installed?.op === "register") return registerRefusedText(installed.where, name);
  // A full sentence that says it was not added and why.
  if (libraryRefusesToo(raw)) return humanizePs5Error(raw);
  return trStatic(
    "queue_register_failed",
    "Couldn't add it to the home screen automatically: {error}. You can still do it from the Library.",
    // The helper's reason in words where it has them.
    { error: humanizePs5Error(raw) },
  );
}
