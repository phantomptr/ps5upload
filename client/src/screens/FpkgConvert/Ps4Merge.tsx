// PS4 updates and add-ons under ① (Convert). An update holds only the files it changed, and a
// game that runs from an image or folder (ShadowMount+) has no update layer, so the update is
// merged into its game: the game and the update are picked, the engine checks they go together
// (same title, newer version) and the image or folder is the game at the update's version. An
// add-on with files becomes a folder; one that is only a licence has nothing to convert.

import { FolderSearch, GitMerge } from "lucide-react";

import type { FpkgInspection } from "../../api/fpkg";
import { Button, Input } from "../../components";
import { useTr } from "../../state/lang";

/** What a PS4 source is, said in a line under the check (null for a PS5 source). */
export function Ps4KindNote({ inspection }: { inspection: FpkgInspection }) {
  const tr = useTr();
  if (!inspection.ps4_game) return null;
  const title = inspection.ps4_title_id ?? "";
  const version = inspection.content_version ?? "";
  let text: string;
  if (inspection.ps4_merge) {
    text = tr(
      "fpkg.ps4_merged_note_v2",
      {
        title: inspection.ps4_merge.title_id,
        from: inspection.ps4_merge.game_version,
        to: inspection.ps4_merge.update_version,
      },
      "The PS4 game {title} with its update merged in: {from} becomes {to}. It converts to a PS4 package, a game image or a folder at that version.",
    );
  } else if (inspection.ps4_kind === "patch") {
    text = tr(
      "fpkg.ps4_patch_note_v2",
      { title, version },
      "A PS4 update, version {version}, for {title}. It holds only the files that changed, so it isn't a game on its own: choose the game it goes into below, and what you make is the game at this version.",
    );
  } else if (inspection.ps4_kind === "addon" && inspection.ps4_addon_data === false) {
    text = tr(
      "fpkg.ps4_licence_note",
      { title },
      "A PS4 add-on for {title} that is only a licence: it unlocks something in the game and holds no files, so there is nothing to convert.",
    );
  } else if (inspection.ps4_kind === "addon") {
    text = tr(
      "fpkg.ps4_addon_note",
      { title },
      "A PS4 add-on for {title}. Its files become a folder, not an image or a package. ShadowMount+ doesn't mount add-ons: they reach the game only where the game itself looks for them, such as a folder inside the game.",
    );
  } else {
    text = tr(
      "fpkg.ps4_game_note_v2",
      undefined,
      "A PS4 game. It converts to a PS4 package, a game image or a folder; an image or folder gets the param.json ShadowMount+ needs.",
    );
  }
  return (
    <div className="text-xs text-[var(--color-muted)]" data-testid="ps4-game-note" data-kind={inspection.ps4_kind ?? "game"}>
      {text}
    </div>
  );
}

/** Which side of a merge the checked source is: an update needs its game, a game may take an
 *  update. Null for anything else. */
export function mergeRole(inspection: FpkgInspection | null): "patch" | "game" | null {
  if (!inspection || inspection.ps4_merge) return null;
  if (inspection.ps4_kind === "patch") return "patch";
  if (inspection.ps4_kind === "game") return "game";
  return null;
}

/** The other half of a merge: the game an update goes into, or an update for the game. */
export function Ps4MergePicker({
  role,
  other,
  onOther,
  onCheck,
  onBrowse,
  merged,
  error,
  checking,
  locked,
}: {
  role: "patch" | "game";
  other: string;
  onOther: (path: string) => void;
  /** Check the pair as it is now (the field was left, or a path was picked). */
  onCheck: () => void;
  onBrowse?: () => void;
  merged: FpkgInspection | null;
  error: string | null;
  checking: boolean;
  locked: boolean;
}) {
  const tr = useTr();
  const m = merged?.ps4_merge;
  return (
    <div className="flex flex-col gap-1.5 border-t border-[var(--color-border)] pt-2.5" data-testid="ps4-merge">
      <div className="flex items-center gap-1.5 text-sm font-medium">
        <GitMerge size={14} aria-hidden className="text-[var(--color-muted)]" />
        {role === "patch"
          ? tr("fpkg.merge.title_patch", undefined, "Merge into the game")
          : tr("fpkg.merge.title_game", undefined, "Merge an update into the game (optional)")}
      </div>
      <div className="flex items-end gap-2">
        <div className="flex-1">
          <Input
            id="fpkg-merge-other"
            label={
              role === "patch"
                ? tr("fpkg.merge.game_label", undefined, "The game this update goes into")
                : tr("fpkg.merge.update_label", undefined, "A PS4 update for this game")
            }
            hint={tr(
              "fpkg.merge.hint",
              undefined,
              "A package, image or folder. The update must be for the same title and newer than the game.",
            )}
            value={other}
            disabled={locked}
            onChange={(e) => onOther(e.target.value)}
            onBlur={onCheck}
          />
        </div>
        {onBrowse && (
          <Button size="sm" variant="secondary" leftIcon={<FolderSearch size={14} />} disabled={locked} onClick={onBrowse}>
            {tr("fpkg.merge.browse", undefined, "Browse")}
          </Button>
        )}
      </div>
      {checking && (
        <div className="text-xs text-[var(--color-muted)]">{tr("fpkg.checking", undefined, "Checking…")}</div>
      )}
      {!checking && error && (
        <div className="text-xs text-[var(--color-warn)]" data-testid="ps4-merge-error">
          {error}
        </div>
      )}
      {!checking && m && (
        <div className="text-xs text-[var(--color-good)]" data-testid="ps4-merge-ok">
          {tr(
            "fpkg.merge.ok",
            { title: m.title ?? m.title_id, from: m.game_version, to: m.update_version, replaced: m.replaced, added: m.added },
            "✓ {title}: {from} becomes {to}. The update replaces {replaced} files and adds {added}.",
          )}
        </div>
      )}
    </div>
  );
}
