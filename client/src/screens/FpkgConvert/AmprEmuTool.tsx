// The AMPR emulator added to a game that is already where it plays. Some games that read through
// libSceAmpr and have none fail to start from an image or folder. A folder takes it in place; an
// image on the PS5, .ffpfsc included, gets a ShadowMount+ backport folder. An image on this
// computer gets it from Convert, which writes a new one.

import { useEffect, useState } from "react";
import { PackagePlus } from "lucide-react";

import { fpkg, type AmprEmuOutcome, type FpkgInspection } from "../../api/fpkg";
import { Button } from "../../components";
import { isRemotePath } from "../../lib/remotePath";
import { isArchiveSource } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";

const IMAGE = /\.(exfat|ffpkg|ufs2|ffpfs|ffpfsc)$/i;

/** Whether the button is offered for `source`, as checked under ①. */
export function amprEmuOffered(source: string, inspection: FpkgInspection | null): boolean {
  const s = source.trim();
  if (!s || !inspection?.ampr_packs?.emulator_missing) return false;
  if (isArchiveSource(s) || /\.pkg$/i.test(s)) return false;
  const onConsole = s.startsWith("ps5://");
  if (!onConsole && isRemotePath(s)) return false;
  return onConsole || !IMAGE.test(s);
}

export function AmprEmuTool({
  source,
  locked,
  onAdded,
}: {
  source: string;
  /** A build is running: one job at a time on this screen. */
  locked: boolean;
  /** The game changed: check it again. */
  onAdded: () => void;
}) {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<AmprEmuOutcome | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    setDone(null);
    setFailed(null);
  }, [source]);
  return (
    <>
      <Button
        size="sm"
        variant="secondary"
        leftIcon={<PackagePlus size={14} />}
        data-testid="ampr-emu-add"
        disabled={locked || busy || !!done}
        title={tr(
          "ampr_emu_add_hint",
          undefined,
          "This game reads its files through AMPR and has no emulator. Some such games won't start from an image without one, and others run fine. Adds drakmor's ampr_emu 0.4.2.1, fetched once and checked against a known digest. An image isn't changed: the emulator goes in a ShadowMount+ backport folder.",
        )}
        onClick={() => {
          setBusy(true);
          setFailed(null);
          fpkg
            .addAmprEmu(source.trim())
            .then((outcome) => {
              setDone(outcome);
              onAdded();
            })
            .catch((e: unknown) => setFailed(e instanceof Error ? e.message : String(e)))
            .finally(() => setBusy(false));
        }}
      >
        {tr("fpkg.image.ampr_emu", undefined, "Add the AMPR emulator")}
      </Button>
      {(done || failed) && (
        <div
          className={`w-full text-xs ${failed ? "text-[var(--color-bad)]" : "text-[var(--color-muted)]"}`}
          data-testid="ampr-emu-status"
          role="status"
        >
          {failed ??
            (done?.kind === "backport"
              ? tr(
                  "ampr_emu_done_backport",
                  { folder: done.folder },
                  "Added as a ShadowMount+ backport in {folder}. The image isn't changed. Start the game to try it.",
                )
              : tr(
                  "ampr_emu_done_folder",
                  { folder: done?.folder ?? "" },
                  "Added to the game's folder, {folder}. Start the game to try it.",
                ))}
        </div>
      )}
    </>
  );
}
