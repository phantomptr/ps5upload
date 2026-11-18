import { HardDrive, PackageOpen, ShieldCheck, Trash2 } from "lucide-react";
import { useNavigate } from "react-router";

import type { GameView } from "../../api/games";
import { Button, Card } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { bestConvertible } from "../../lib/collectionConvert";
import { extraCopies, formatCollectionBytes } from "../../lib/collectionView";
import { isRemotePath } from "../../lib/remotePath";
import { useCollectionStore } from "../../state/collection";
import { useTr } from "../../state/lang";
import { trashCopies } from "../Collection/trashCopies";
import { CopyRow } from "./CopyRow";
import { useSmpState } from "./SmpNeeded";
import { firmwareShortfall } from "../../lib/collectionInsights";
import { parsePS5Firmware } from "../../lib/ps5Firmware";
import { useConnectionStore } from "../../state/connection";
import { refreshCopyChecks, startCopyChecks } from "../../state/copyChecks";
import { useEffect } from "react";

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
  /** Opens the connected console's send panel with that copy picked. */
  onSend: (path: string) => void;
  /** After copies moved to the Trash. */
  onChanged: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const settings = useCollectionStore((s) => s.settings);
  const canTrash =
    settings?.trash_available !== false || !!settings?.allow_permanent_delete;
  const copies = view?.copies ?? [];
  // A package row's button depends on whether Kraken Mount runs there: read it once.
  useSmpState(connectedHost && copies.some((l) => l.type === "pkg") ? connectedHost : "");
  const trash = (paths: string[]) =>
    void trashCopies(paths, confirm, tr).then(onChanged);
  const extra = extraCopies({ locations: copies }).filter(
    (l) => !isRemotePath(l.absolute_path),
  );
  const extraBytes = extra.reduce((n, l) => n + l.size_bytes, 0);
  // The copy Convert opens with: a folder before an image, archive or base package.
  const convertible = bestConvertible(copies);
  // Against the connected console's firmware: a game that asks for newer one may not start.
  const kernel = useConnectionStore((s) => s.ps5Kernel);
  const consoleFw = connectedHost ? parsePS5Firmware(kernel) : null;
  const need = view
    ? firmwareShortfall(
        { platform: view.platform, locations: copies },
        consoleFw,
      )
    : null;
  // Checks started elsewhere (the Collection's menu or selection) show here too.
  useEffect(() => {
    void refreshCopyChecks();
  }, []);
  return (
    <Card>
      {dialog}
      <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
        <h2 className="flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
            <HardDrive size={15} aria-hidden />
          </span>
          {tr(
            "game_computer_title",
            { n: copies.length },
            "On this computer ({n})",
          )}
        </h2>
        <span className="flex flex-wrap items-center gap-2">
          {copies.length > 1 && (
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<ShieldCheck size={13} />}
              onClick={() =>
                void startCopyChecks(copies.map((l) => l.absolute_path))
              }
              title={tr(
                "collection.check_hint",
                undefined,
                "Read every copy back to find one that is damaged or cut short. Runs on this computer, one copy at a time.",
              )}
            >
              {tr("copycheck.all", undefined, "Check all copies")}
            </Button>
          )}
          {convertible && (
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<PackageOpen size={13} />}
              data-testid="game-convert"
              onClick={() =>
                navigate("/workshop", {
                  state: { source: convertible.absolute_path },
                })
              }
              title={tr(
                "collection.convert_hint_v2",
                undefined,
                "Open it in Workshop: make an installable package, or a different game image.",
              )}
            >
              {tr("collection.convert", undefined, "Convert…")}
            </Button>
          )}
        </span>
      </div>
      {need && consoleFw && (
        <p className="mb-3 text-sm text-[var(--color-warn)]">
          {tr(
            "collection.fw_hint",
            { need, have: consoleFw },
            "It asks for firmware {need}; this PS5 is on {have}. It may not start unless it is backported.",
          )}
        </p>
      )}
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
                {tr(
                  "collection.keep_largest",
                  { size: formatCollectionBytes(extraBytes) },
                  "Keep the largest copy, free {size}",
                )}
              </Button>
            </div>
          )}
          <ul className="flex flex-col gap-1.5" data-testid="game-copies">
            {copies.map((l) => (
              <CopyRow
                key={`${l.root}|${l.path}`}
                loc={l}
                host={connectedHost}
                game={{
                  title: view?.title ?? "",
                  titleId: view?.title_id ?? "",
                }}
                onTrash={canTrash ? trash : undefined}
                onSend={() => onSend(l.absolute_path)}
              />
            ))}
          </ul>
        </>
      )}
    </Card>
  );
}
