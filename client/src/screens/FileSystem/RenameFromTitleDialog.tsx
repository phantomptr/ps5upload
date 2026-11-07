// Rename a game from what it says about itself: its title, title ID, version and format, by
// formats (lib/nameFormats). A name ShadowMount+ wouldn't mount is flagged, with a shortened twin.

import { useEffect, useMemo, useState } from "react";

import { fpkg, type FpkgInspection } from "../../api/fpkg";
import { Button, Modal, Spinner } from "../../components";
import { humanizeCaught } from "../../lib/humanizeError";
import {
  DEFAULT_NAME_FORMATS,
  fittedName,
  formatOfName,
  nameFormatProblem,
  nameOverLimit,
  shownName,
  suggestedName,
  type NameValues,
} from "../../lib/nameFormats";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { useTr } from "../../state/lang";
import { titleIdOf } from "../FpkgConvert/GameCard";

/** The values of a game, from its check. */
export function valuesOf(inspection: FpkgInspection, name: string, folder: boolean): NameValues {
  const plain = shownName(name);
  const dot = plain.lastIndexOf(".");
  return {
    title: inspection.title,
    titleid: titleIdOf(inspection.content_id),
    contentid: inspection.content_id,
    version: inspection.content_version,
    format: formatOfName(name, folder),
    platform: inspection.ps4_game ? "PS4" : "PS5",
    name: !folder && dot > 0 ? plain.slice(0, dot) : plain,
  };
}

const KEY_OWN_FORMAT = "ps5upload.files.rename_format";
const OWN_FORMAT_EXAMPLE = "{title} [{titleid}] <v{version:short}>";

/** Every name the formats give, once each, then shortened twins of those too long to mount. */
export function nameChoices(
  values: NameValues,
  name: string,
  folder: boolean,
  own?: string,
): string[] {
  const out: string[] = [];
  const add = (n: string | null) => {
    if (n && n !== name && !out.includes(n)) out.push(n);
  };
  const formats = own?.trim() ? [own, ...DEFAULT_NAME_FORMATS] : DEFAULT_NAME_FORMATS;
  for (const f of formats) {
    add(suggestedName(f, values, name, folder));
    add(fittedName(f, values, name, folder));
  }
  return out;
}

export function RenameFromTitleDialog({
  source,
  name,
  folder,
  onCancel,
  onRename,
}: {
  /** The game, as Convert reads it: `ps5://host/path`. */
  source: string;
  name: string;
  folder: boolean;
  onCancel: () => void;
  onRename: (newName: string) => void;
}) {
  const tr = useTr();
  const [inspection, setInspection] = useState<FpkgInspection | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [draft, setDraft] = useState(name);
  const [own, setOwn] = useState(() => safeGetItem(KEY_OWN_FORMAT) ?? "");
  const ownProblem = own.trim() ? nameFormatProblem(own) : null;
  useEffect(() => {
    let live = true;
    fpkg
      .inspect(source)
      .then((i) => {
        if (live) setInspection(i);
      })
      .catch((e: unknown) => {
        if (live) setFailed(humanizeCaught(e));
      });
    return () => {
      live = false;
    };
  }, [source]);
  const choices = useMemo(
    () => (inspection ? nameChoices(valuesOf(inspection, name, folder), name, folder, own) : []),
    [inspection, name, folder, own],
  );
  const over = nameOverLimit(draft.trim());
  return (
    <Modal
      open
      onClose={onCancel}
      title={tr("fs_rename_from_title", undefined, "Rename from title")}
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={!draft.trim() || draft.trim() === name}
            data-testid="rename-title-ok"
            onClick={() => onRename(draft.trim())}
          >
            {tr("fs_rename", undefined, "Rename")}
          </Button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {!inspection && !failed && <Spinner size={16} />}
        {failed && <p className="text-[var(--color-bad)]">{failed}</p>}
        {inspection && choices.length === 0 && (
          <p className="text-[var(--color-muted)]">
            {tr("fs_rename_no_title", undefined, "This doesn't say its title or title ID, so there's nothing to name it from.")}
          </p>
        )}
        <div className="space-y-1" data-testid="rename-title-choices">
          {choices.map((c) => (
            <button
              key={c}
              type="button"
              aria-pressed={draft === c}
              onClick={() => setDraft(c)}
              className={`block w-full break-all rounded-[var(--radius-field)] border px-3 py-1.5 text-start text-xs ${
                draft === c ? "border-[var(--color-accent)] bg-[var(--color-surface-raised)]" : "border-[var(--color-border)]"
              }`}
            >
              {c}
            </button>
          ))}
        </div>
        <label className="block text-xs">
          <span className="mb-1 block font-medium">{tr("fs_rename_format", undefined, "Your own format")}</span>
          <input
            className="input w-full"
            value={own}
            placeholder={OWN_FORMAT_EXAMPLE}
            data-testid="rename-title-format"
            onChange={(e) => {
              setOwn(e.target.value);
              safeSetItem(KEY_OWN_FORMAT, e.target.value);
            }}
          />
          <span className={`mt-1 block ${ownProblem ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}>
            {ownProblem ??
              tr(
                "fs_rename_format_hint",
                undefined,
                "Fields: {title} {titleid} {contentid} {version} {format} {platform} {name}. Modifiers: {title:upper}, {title:20}, {version:short}. <...> is left out when a value in it is missing. Remembered on this device.",
              )}
          </span>
        </label>
        <input
          className="input w-full"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          aria-label={tr("fs_rename", undefined, "Rename")}
          data-testid="rename-title-input"
        />
        {over > 0 && (
          <p className="text-xs text-[var(--color-warn)]" data-testid="rename-title-long">
            {tr(
              "fs_name_too_long_smp",
              { over },
              "{over} bytes too long for ShadowMount+ to mount. It won't show up on the console under this name.",
            )}
          </p>
        )}
      </div>
    </Modal>
  );
}
