import { HardDrive, Trash2 } from "lucide-react";

import type { GameView } from "../../api/games";
import { Button, Card } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { extraCopies, formatCollectionBytes } from "../../lib/collectionView";
import { isRemotePath } from "../../lib/remotePath";
import { useCollectionStore } from "../../state/collection";
import { useTr } from "../../state/lang";
import { trashCopies } from "../Collection/trashCopies";
import { CopyRow } from "./CopyRow";

/** Every copy of the game on this computer's drives and saved servers. */
export function DrivesCard({
  view,
  connectedHost,
  onSend,
  onChanged,
}: {
  view: GameView | null;
  /** The connected console when its helper answers; "" otherwise. */
  connectedHost: string;
  /** Opens the connected console's send panel. */
  onSend: () => void;
  /** After copies moved to the Trash. */
  onChanged: () => void;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const settings = useCollectionStore((s) => s.settings);
  const canTrash = settings?.trash_available !== false || !!settings?.allow_permanent_delete;
  const copies = view?.copies ?? [];
  const trash = (paths: string[]) => void trashCopies(paths, confirm, tr).then(onChanged);
  const extra = extraCopies({ locations: copies }).filter((l) => !isRemotePath(l.absolute_path));
  const extraBytes = extra.reduce((n, l) => n + l.size_bytes, 0);
  return (
    <Card>
      {dialog}
      <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold">
        <HardDrive size={16} className="text-[var(--color-muted)]" />
        {tr("game_drives_title", { n: copies.length }, "On your drives ({n})")}
      </h2>
      {copies.length === 0 ? (
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "game_drives_none",
            undefined,
            "No copy on this computer's drives. Add the folder it is in to the Collection to see it here.",
          )}
        </p>
      ) : (
        <>
          {canTrash && extra.length > 0 && (
            <div className="mb-3">
              <Button
                size="sm"
                variant="danger"
                leftIcon={<Trash2 size={13} />}
                onClick={() => trash(extra.map((l) => l.absolute_path))}
                title={tr(
                  "collection.keep_largest_hint",
                  undefined,
                  "Keeps the largest full copy and moves the other copies to the Trash. Updates and DLC stay.",
                )}
              >
                {tr("collection.keep_largest", { size: formatCollectionBytes(extraBytes) }, "Keep the largest copy, free {size}")}
              </Button>
            </div>
          )}
          <ul className="flex flex-col gap-2" data-testid="game-copies">
            {copies.map((l) => (
              <CopyRow
                key={`${l.root}|${l.path}`}
                loc={l}
                host={connectedHost}
                onTrash={canTrash ? trash : undefined}
                onSend={onSend}
              />
            ))}
          </ul>
        </>
      )}
    </Card>
  );
}
