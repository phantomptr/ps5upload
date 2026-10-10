// The viewer's Related tab: the title's other packages in the library and the queue.

import type { RelatedKind, relatedPackages } from "../../lib/viewerLists";
import { useTr } from "../../state/lang";
import { Button } from "../index";

export function RelatedTab(props: {
  groups: ReturnType<typeof relatedPackages>;
  /** Open a library package in the viewer; absent where the screen can't. */
  onOpen?: (path: string) => void;
}) {
  const tr = useTr();
  const heading: Record<RelatedKind, string> = {
    game: tr("viewer_type_game", undefined, "Game"),
    update: tr("viewer_type_update", undefined, "Update"),
    dlc: tr("viewer_type_dlc", undefined, "DLC"),
    other: tr("viewer_related_other", undefined, "Other"),
  };
  if (props.groups.length === 0) {
    return (
      <div className="text-sm text-[var(--color-muted)]">
        {tr("viewer_related_none", undefined, "No other packages of this title in the library or the queue.")}
      </div>
    );
  }
  return (
    <div className="grid gap-4">
      {props.groups.map((g) => (
        <section key={g.kind} className="grid gap-1">
          <h3 className="text-xs font-medium uppercase tracking-wide text-[var(--color-muted)]">{heading[g.kind]}</h3>
          <ul className="grid gap-1">
            {g.items.map((i) => (
              <li
                key={`${i.where}:${i.path ?? i.name}`}
                className="flex items-center gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] px-3 py-2 text-sm"
              >
                <span className="min-w-0 flex-1 truncate">{i.name}</span>
                {i.version && <span className="font-mono text-xs text-[var(--color-muted)]">{i.version}</span>}
                <span className="rounded bg-[var(--color-surface-2)] px-1.5 py-0.5 text-xs text-[var(--color-muted)]">
                  {i.where === "library"
                    ? tr("viewer_related_library", undefined, "Library")
                    : tr("viewer_related_queue", undefined, "Queue")}
                </span>
                {i.path && props.onOpen && (
                  <Button size="sm" variant="ghost" onClick={() => props.onOpen!(i.path!)}>
                    {tr("viewer_related_view", undefined, "View")}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}
