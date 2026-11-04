// The organizer's preview, as the modal works with it: a split package's parts move as one, so a
// tick stands for all of them, and the running plan says how far it is.

import type { OrganizeMove, OrganizePlanStatus } from "../../api/collection";
import type { Translator } from "../../state/lang";

export const moveKey = (m: OrganizeMove) => `${m.container}|${m.from}`;

/** `unticked` with `m` ticked (`on`) or not, and every other part of its split set the same. */
export function toggleMove(
  unticked: ReadonlySet<string>,
  moves: readonly OrganizeMove[],
  m: OrganizeMove,
  on: boolean,
): Set<string> {
  const next = new Set(unticked);
  const same = m.set
    ? moves.filter((x) => x.container === m.container && x.set === m.set)
    : [m];
  for (const x of same) {
    if (on) next.delete(moveKey(x));
    else next.add(moveKey(x));
  }
  return next;
}

/** What a running preview is doing, in a line. */
export function planProgressText(tr: Translator, st: OrganizePlanStatus | null): string {
  if (st?.phase === "reading" && st.total > 0) {
    return tr(
      "collection.org_plan_reading",
      { done: st.done, total: st.total },
      "Reading items: {done} of {total}",
    );
  }
  if (st?.phase === "finding") {
    return tr(
      "collection.org_plan_finding",
      undefined,
      "Looking through your folders…",
    );
  }
  return tr("collection.org_planning", undefined, "Reading collection items…");
}
