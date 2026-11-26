// Choices under ③ Options that are about the one game checked under ①, and are never
// remembered: they are off again for the next game.

import { useEffect, useRef, useState } from "react";

import { fpkg, type ArchiveFreeCheck, type BackportInfo } from "../../api/fpkg";
import { Toggle } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { archiveErrorText, archiveIsUnpacked } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";

/** Why the game's backport cannot be left out of what is being made now, or null when it can:
 *  the engine's own reason (the backport changed eboot.bin), or a game in asset packs that
 *  this run does not unpack (`unpacks`: it writes new zstd packs, which decodes the old ones). */
export function backportRefusal(backport: BackportInfo, unpacks: boolean): "blocked" | "packed" | null {
  if (backport.blocked) return "blocked";
  return backport.packed && !unpacks ? "packed" : null;
}

/** "Remove backport libraries": shown for a game whose fakelib holds backport libraries, when
 *  an image or a folder is made of it. The emulators in fakelib are kept, and the source is
 *  not changed; a game the backport changed more deeply says why it stays. */
export function RemoveBackportOption({
  backport,
  unpacks,
  checked,
  onChange,
}: {
  backport: BackportInfo;
  /** This run writes zstd packs, so a packed game is unpacked on the way. */
  unpacks: boolean;
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
  const tr = useTr();
  const refusal = backportRefusal(backport, unpacks);
  const count = backport.libraries.length;
  // The kinds of emulator kept, each once (two AMPR files are one emulator to the reader).
  const kept = [...new Set(backport.emulators.map((e) => e.name))].join(", ");
  return (
    <div className="space-y-1" data-testid="remove-backport-option">
      <Toggle
        checked={checked && !refusal}
        onChange={onChange}
        disabled={!!refusal}
        label={tr("fpkg.backport.remove", undefined, "Remove backport libraries")}
        hint={tr(
          "fpkg.backport.remove_body",
          { count },
          "Leaves the {count} backport libraries in this game's fakelib out of what is made, for a console whose own firmware runs the game. The game you chose is not changed.",
        )}
      />
      {kept && (
        <div className="text-xs text-[var(--color-muted)]" data-testid="remove-backport-kept">
          {tr("fpkg.backport.kept", { names: kept }, "Emulators in fakelib are kept: {names}.")}
        </div>
      )}
      {refusal === "blocked" && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="remove-backport-blocked">
          {tr(
            "fpkg.backport.blocked",
            { reason: backport.blocked ?? "" },
            "Not available for this game: {reason}",
          )}
        </div>
      )}
      {refusal === "packed" && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="remove-backport-packed">
          {tr(
            "fpkg.backport.packed",
            undefined,
            "This game is in asset packs, and their own list names the backport libraries. Turn on zstd asset packs to leave them out: that unpacks the game first.",
          )}
        </div>
      )}
    </div>
  );
}

/** Why the source cannot be moved to the Trash after this conversion, or null when it can. */
export type DeleteSourceRefusal =
  | { kind: "engine"; reason: string }
  | { kind: "archive" | "merge" | "elsewhere" };

/** The first thing in the way of "move the source to the Trash": the engine's own reason (the
 *  source is on a PS5 or a server, or there is no Trash), an archive (what is converted is its
 *  scratch copy), an update merged in (two sources), or an output that is not kept on this
 *  computer (saved straight to the PS5 or a server). The engine checks all of this again, and
 *  that the output is not inside the source, before anything is written. */
export function deleteSourceRefusal(o: {
  engineReason?: string | null;
  archive: boolean;
  merge: boolean;
  elsewhere: boolean;
}): DeleteSourceRefusal | null {
  if (o.engineReason) return { kind: "engine", reason: o.engineReason };
  if (o.archive) return { kind: "archive" };
  if (o.merge) return { kind: "merge" };
  return o.elsewhere ? { kind: "elsewhere" } : null;
}

/** "Move the source to the Trash once the output is verified": for an image or folder made of
 *  a file or folder on this computer. Off unless ticked for this one conversion, and ticking
 *  it asks first, naming the path (the screen does that). */
export function DeleteSourceOption({
  path,
  refusal,
  checked,
  onChange,
}: {
  /** What would go. */
  path: string;
  refusal: DeleteSourceRefusal | null;
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
  const tr = useTr();
  const why =
    refusal?.kind === "engine"
      ? tr("fpkg.delete_source.no_engine", { reason: refusal.reason }, "Not available: {reason}")
      : refusal?.kind === "archive"
        ? tr(
            "fpkg.delete_source.no_archive",
            undefined,
            "Not available for an archive: it is unpacked to a scratch copy first, and the archive itself is kept.",
          )
        : refusal?.kind === "merge"
          ? tr(
              "fpkg.delete_source.no_merge",
              undefined,
              "Not available with an update merged in: there are two sources.",
            )
          : refusal?.kind === "elsewhere"
            ? tr(
                "fpkg.delete_source.no_elsewhere",
                undefined,
                "Not available when the output is saved straight to the PS5 or a server: only for an output kept on this computer.",
              )
            : null;
  const on = checked && !refusal;
  return (
    <div className="space-y-1" data-testid="delete-source-option">
      <Toggle
        checked={on}
        onChange={onChange}
        disabled={!!refusal}
        label={tr("fpkg.delete_source", undefined, "Move the source to the Trash once the output is verified")}
        hint={tr(
          "fpkg.delete_source_body",
          undefined,
          "After the output is written, every byte of it is read back and checked, and only then is the game you chose moved to the Trash or Recycle Bin. It stays if the conversion fails or is stopped, or if it changed meanwhile. For this conversion only: it is off again afterwards, and games in the queue are never removed.",
        )}
      />
      {on && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="delete-source-path">
          {tr("fpkg.delete_source.path", { path }, "Will be moved to the Trash: {path}")}
        </div>
      )}
      {why && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="delete-source-refused">
          {why}
        </div>
      )}
    </div>
  );
}

/** "Free the archive's space while it unpacks": for an archive on this computer that the run
 *  unpacks to disk (a package of it always; an image or folder only when the game inside is a
 *  game image or an update is merged in). It DELETES the archive as it unpacks, so it is off
 *  unless turned on for this one game, and turning it on first asks the engine whether and how
 *  it can be done (`fpkg.freeCheck`, which changes nothing) and then asks the user, with what
 *  is lost if anything goes wrong. Never remembered: the screen holds the answer for the one
 *  source it was given for. */
export function FreeArchiveOption({
  source,
  outputDir,
  password,
  kind,
  merge = false,
  checked,
  onChange,
}: {
  /** The archive that would go. */
  source: string;
  /** Where the game is unpacked (and the room is checked). */
  outputDir?: string;
  /** A .rar's password, for the check. */
  password?: string;
  /** What is being made of it. */
  kind: "pkg" | "image" | "folder";
  /** An update is merged in (the game is unpacked for that). */
  merge?: boolean;
  /** The user said yes for this source. */
  checked: boolean;
  /** `check`: what the engine found, when turned on. */
  onChange: (on: boolean, check?: ArchiveFreeCheck) => void;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  // What the engine said about this archive, for the notes under the switch.
  const [check, setCheck] = useState<ArchiveFreeCheck | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  // Only the answer for the archive shown now may land.
  const asked = useRef(0);
  useEffect(() => {
    asked.current += 1;
    setCheck(null);
    setRefusal(null);
    setChecking(false);
  }, [source]);

  // Known not to be unpacked for what is being made: there is nothing to free.
  const notNeeded = check !== null && !archiveIsUnpacked(source, kind, { merge, game: check.game });
  const on = checked && !notNeeded && !refusal;

  const turnOn = async () => {
    const mine = ++asked.current;
    setRefusal(null);
    setChecking(true);
    let found: ArchiveFreeCheck;
    try {
      found = await fpkg.freeCheck(source, outputDir, password || undefined);
    } catch (e) {
      if (mine !== asked.current) return;
      setChecking(false);
      setRefusal(archiveErrorText(e instanceof Error ? e.message : String(e)));
      return;
    }
    if (mine !== asked.current) return;
    setChecking(false);
    setCheck(found);
    if (!archiveIsUnpacked(source, kind, { merge, game: found.game })) return;
    const how =
      found.mode === "entries"
        ? tr("fpkg.free.how_entries", undefined, "A .zip is freed file by file as it unpacks.")
        : found.mode === "blocks"
          ? tr("fpkg.free.how_blocks", { count: found.parts }, "This .7z is in {count} blocks. Each block is deleted once everything in it is unpacked, starting from the end of the archive.")
          : found.mode === "parts"
          ? tr(
              "fpkg.free.how_parts",
              { count: found.parts },
              "This archive has {count} parts. Each part is deleted once everything in it is unpacked; a file that runs across several parts keeps them until it is whole.",
            )
          : tr("fpkg.free.end_note", undefined, "This kind of archive can't be freed bit by bit: until unpacking is done there has to be room for the archive and the unpacked game together.");
    const atEnd = found.mode === "end";
    const ok = await confirm({
      title: atEnd
        ? tr("fpkg.free.confirm_title_end", undefined, "Delete this archive once it is unpacked?")
        : tr("fpkg.free.confirm_title", undefined, "Delete this archive while it unpacks?"),
      message: `${
        atEnd
          ? tr(
              "fpkg.free.confirm_body_end",
              undefined,
              "This deletes the archive as soon as it is unpacked and checked, to make room for the next step. The archive is deleted for good, not moved to the Trash. If the steps after unpacking fail or are stopped, the archive is already gone and can't be brought back; you keep the unpacked game. Only use this for an archive you can get again.",
            )
          : tr(
              "fpkg.free.confirm_body",
              undefined,
              "This frees the archive's space while it unpacks, so the job fits on a smaller drive. The archive is deleted for good, not moved to the Trash. If unpacking or the steps after it fail or are stopped, the archive is already gone and can't be brought back; you keep whatever was unpacked so far. Only use this for an archive you can get again.",
            )
      } ${how}`,
      details: [source],
      confirmLabel:
        found.mode === "end"
          ? tr("fpkg.free.confirm_yes_end", undefined, "Delete it once it is unpacked")
          : tr("fpkg.free.confirm_yes", undefined, "Delete it while it unpacks"),
      destructive: true,
    });
    if (mine !== asked.current) return;
    if (ok) onChange(true, found);
  };

  return (
    <div className="space-y-1" data-testid="free-archive-option">
      <Toggle
        checked={on}
        disabled={checking}
        onChange={(next) => {
          if (next) void turnOn();
          else onChange(false);
        }}
        label={
          <span>
            {tr("fpkg.free.label", undefined, "Free the archive's space while it unpacks")}{" "}
            <span
              className="rounded border border-[var(--color-bad)] px-1 py-px text-[10px] font-semibold uppercase text-[var(--color-bad)]"
              data-testid="free-archive-danger"
            >
              {tr("fpkg.free.danger", undefined, "Dangerous")}
            </span>
          </span>
        }
        hint={tr(
          "fpkg.free.hint",
          undefined,
          "For a drive with no room for the archive and the unpacked game together. The archive is deleted as it unpacks and can't be brought back. If anything fails on the way, you keep only what was unpacked so far. For this game only: it is off again afterwards.",
        )}
      />
      {checking && (
        <div className="text-xs text-[var(--color-muted)]" data-testid="free-archive-checking">
          {tr("fpkg.free.checking", undefined, "Checking the archive…")}
        </div>
      )}
      {on && check && (
        <div className="text-xs text-[var(--color-bad)]" data-testid="free-archive-path">
          {check.mode === "end"
            ? tr("fpkg.free.path_end", { path: source }, "Will be deleted as soon as it is unpacked: {path}")
            : tr("fpkg.free.path", { path: source }, "Will be deleted while it unpacks: {path}")}
        </div>
      )}
      {on && check?.mode === "end" && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="free-archive-end-note">
          {tr("fpkg.free.end_note", undefined, "This kind of archive can't be freed bit by bit: until unpacking is done there has to be room for the archive and the unpacked game together.")}
        </div>
      )}
      {notNeeded && (
        <div className="text-xs text-[var(--color-muted)]" data-testid="free-archive-not-needed">
          {tr(
            "fpkg.free.not_needed",
            undefined,
            "Not needed here: an image or a folder is made straight from this archive with nothing unpacked, so there is no extra space to free. It applies when a package is made.",
          )}
        </div>
      )}
      {refusal && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="free-archive-refused">
          {tr("fpkg.free.refused", { reason: refusal }, "Not available: {reason}")}
        </div>
      )}
      {dialog}
    </div>
  );
}
