import { useState } from "react";

import { Button, Modal } from "../../components";
import { useTr } from "../../state/lang";
import type { ExportDest } from "./BackupLayoutChoice";

/** Where "Save as a file for a friend or another PS5" puts the file when a backup place is
 *  arranged by user (#886): that user's Decrypted folder there (the first choice), or this device
 *  as before. */
export default function ExportDestDialog({
  game,
  who,
  dests,
  onPick,
  onClose,
}: {
  game: string;
  /** The save's owner, by name. */
  who: string;
  /** From `exportDests`: never empty. */
  dests: ExportDest[];
  onPick: (dest: ExportDest) => void;
  onClose: () => void;
}) {
  const tr = useTr();
  const [dest, setDest] = useState<ExportDest>(dests[0] ?? "device");
  const label = (d: ExportDest) =>
    d === "local"
      ? tr("saves_export_local", undefined, "In the backup folder")
      : d === "server"
        ? tr("saves_export_server", undefined, "On the network share")
        : tr("saves_export_device", undefined, "On this device");
  const desc = (d: ExportDest) =>
    d === "device"
      ? tr("saves_export_device_desc", undefined, "Where your downloads go, as before.")
      : tr("saves_export_user_desc", { who }, "In {who}'s Decrypted folder, under PS5 and the game.");
  return (
    <Modal
      open
      onClose={onClose}
      title={tr("saves_export_title", { game }, "Save {game}'s save as a file")}
      size="md"
      bodyClassName="p-4 sm:p-5"
      footer={
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button size="sm" onClick={() => onPick(dest)} data-testid="save-export-go">
            {tr("save", undefined, "Save")}
          </Button>
        </div>
      }
    >
      <fieldset className="flex flex-col gap-2" data-testid="save-export-dests">
        <legend className="mb-1 text-xs text-[var(--color-muted)]">
          {tr(
            "saves_export_note",
            undefined,
            "The file is not locked to this PS5: it loads on a friend's or your other PS5.",
          )}
        </legend>
        {dests.map((d) => (
          <label
            key={d}
            className="flex cursor-pointer items-start gap-2 rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm"
          >
            <input
              type="radio"
              name="save-export-dest"
              value={d}
              checked={dest === d}
              onChange={() => setDest(d)}
              data-testid={`save-export-${d}`}
              className="mt-1"
            />
            <span>
              <span className="block">{label(d)}</span>
              <span className="block text-xs text-[var(--color-muted)]">{desc(d)}</span>
            </span>
          </label>
        ))}
      </fieldset>
    </Modal>
  );
}
