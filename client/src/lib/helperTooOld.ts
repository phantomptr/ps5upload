// A helper that runs on the PS5 but is too old for this app (#877), told apart from no helper
// at all. The engine still reports both as `helper_not_ava1` (what older clients key on) and
// adds `helper_too_old` when a helper answered; the screens that say "helper isn't running"
// say this instead, with the same Send helper action.

import type { Translator } from "../state/lang";

/** Whether a console's "down" is an older helper that runs, rather than nothing running. */
export function showsHelperTooOld(rt: {
  payloadStatus: "up" | "down" | "unknown";
  helperTooOld?: boolean;
}): boolean {
  return rt.payloadStatus === "down" && rt.helperTooOld === true;
}

/** The title and the next step for an older helper. The engine does not learn the old
 *  helper's version (it stops at the session's capability bits), so none is named. */
export function helperTooOldText(tr: Translator): { title: string; detail: string } {
  return {
    title: tr("helper_too_old_title", undefined, "An older PS5 helper is running"),
    detail: tr(
      "helper_too_old_detail",
      undefined,
      "The PS5 runs an older helper, which this app can't use. Send the helper from here to replace it. If a payload loader starts the helper when the PS5 boots, update the copy it uses.",
    ),
  };
}
