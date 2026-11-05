// A queued game's own choices (#518, #669): everything it was added with, changeable while it
// waits, the queue running or not. Cards ② and ③ set them for games as they are added; this is
// for one already in the queue.

import { Button } from "../../components";
import { SegmentedControl } from "../../components/SegmentedControl";
import { Toggle } from "../../components/Toggle";
import { parentDir } from "../../lib/outputPlace";
import { PLAYGO_LANGUAGES } from "../../lib/playgoLanguages";
import { useConvertPrefs } from "../../state/convertPrefs";
import { thenFor, type ConvertBuild, type ConvertItem, type ConvertItemChange, type ConvertThen } from "../../state/convertQueue";
import { isArchiveSource, isFolderableSource } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import { CompressionTiles } from "./CompressionTiles";
import { imageDestFor } from "../../lib/imageDest";
import { ImageDestination } from "./ImageDestination";
import { FullVerifyOption, ImageChoice } from "./ImageOptions";

type Kind = ConvertBuild["kind"];

/** What this source can become: a folder only from an image or package; an image from a game
 *  folder, image, package or archive (a file of any other kind cannot). */
export function kindsFor(source: string, isDir?: boolean): Kind[] {
  // Known to be a folder when added; a name is only a guess (`Some.Game.2024` is a folder).
  const file = !isDir && /\.[a-z0-9]{2,6}$/i.test(source.trim());
  const kinds: Kind[] = ["pkg"];
  if (!file || isFolderableSource(source) || isArchiveSource(source)) kinds.push("image");
  if (isFolderableSource(source)) kinds.push("folder");
  return kinds;
}

/** The change for choosing `kind`: an image keeps its own format choice when it was one. */
export function changeForKind(item: ConvertItem, kind: Kind, canInstall: boolean, host: string | null): ConvertItemChange {
  const build: ConvertBuild =
    kind === "image"
      ? item.build?.kind === "image"
        ? item.build
        : { kind: "image", format: "exfat", compress: false }
      : { kind };
  const then = thenFor(kind, item.then, canInstall);
  return { build, then, host: then !== "keep" ? (item.host ?? host) : null };
}

/** The change for "Save next to its source" on one game: back to the Output folder when off. A
 *  game with no folder here to go next to (on a server or the PS5) stays where it goes. */
export function changeForBeside(source: string, on: boolean, outputFolder: string): ConvertItemChange {
  if (!on) return { outputDir: outputFolder.trim() || undefined, keepBoth: undefined };
  const parent = parentDir(source);
  return parent ? { outputDir: parent, keepBoth: true } : {};
}

export function QueueItemOptions({
  item,
  canInstall,
  host,
  canBrowse = false,
  onPickOutput,
  onChange,
}: {
  item: ConvertItem;
  canInstall: boolean;
  host: string | null;
  /** A folder picker exists here. */
  canBrowse?: boolean;
  /** Pick the output folder; null when the picker was closed. */
  onPickOutput?: () => Promise<string | null>;
  onChange: (change: ConvertItemChange) => void;
}) {
  const tr = useTr();
  const rememberedOutput = useConvertPrefs((s) => s.outputDir);
  const rememberedVerify = useConvertPrefs((s) => s.fullVerify);
  const rememberedLevel = useConvertPrefs((s) => s.imageLevel);
  const kind: Kind = item.build?.kind ?? "pkg";
  const kinds = kindsFor(item.source, item.isDir);
  const kindLabel: Record<Kind, string> = {
    pkg: tr("cq_opt_pkg", undefined, "Package"),
    image: tr("cq_opt_image", undefined, "Game image"),
    folder: tr("cq_opt_folder", undefined, "Folder"),
  };
  // As ④ words them: a package streams or uploads, an image uploads, a folder stays.
  const thens: ConvertThen[] =
    kind === "folder" || !canInstall ? ["keep"] : kind === "image" ? ["keep", "upload"] : ["keep", "stream", "upload"];
  const thenLabel: Record<ConvertThen, string> =
    kind === "folder"
      ? {
          keep: tr("cq_then_keep_folder", undefined, "Keep the folder"),
          stream: "",
          upload: "",
        }
      : kind === "image"
        ? {
            keep: tr("cq_then_keep_image", undefined, "Keep the image"),
            stream: "",
            upload: tr("cq_then_upload_image", undefined, "Upload it to the PS5"),
          }
        : {
            keep: tr("cq_then_keep", undefined, "Keep the package"),
            stream: tr("cq_then_stream", undefined, "Stream & install"),
            upload: tr("cq_then_upload", undefined, "Upload & install"),
          };
  const beside = !!item.keepBoth;
  const canBeside = parentDir(item.source) !== null;
  const outputId = `cq-output-${item.id}`;
  const languageId = `cq-language-${item.id}`;
  return (
    <div
      className="flex flex-col gap-3 border-t border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-3 text-sm"
      data-testid="queue-item-options"
    >
      <div className="flex flex-col gap-1">
        <span className="text-xs text-[var(--color-muted)]">{tr("cq_opt_make", undefined, "Make")}</span>
        <SegmentedControl
          value={kind}
          ariaLabel={tr("cq_opt_make", undefined, "Make")}
          segments={kinds.map((k) => ({ value: k, label: kindLabel[k] }))}
          onChange={(k) => onChange(changeForKind(item, k as Kind, canInstall, host))}
        />
      </div>

      {item.build?.kind === "image" && (
        <ImageChoice
          format={item.build.format}
          compress={item.build.compress}
          onChoice={(format, compress) => onChange({ build: { kind: "image", format, compress } })}
          level={{ value: item.imageLevel ?? rememberedLevel, onChange: (imageLevel) => onChange({ imageLevel }) }}
        />
      )}

      {kind === "pkg" && (
        <>
          <div className="flex flex-col gap-1">
            <span className="text-xs text-[var(--color-muted)]">{tr("fpkg.compression", undefined, "Compression")}</span>
            <CompressionTiles value={item.compression} onChange={(compression) => onChange({ compression })} estimates={null} />
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <label htmlFor={languageId} className="text-xs text-[var(--color-muted)]">
              {tr("fpkg.language", undefined, "Game language")}
            </label>
            <select
              id={languageId}
              className="select w-auto!"
              value={item.language ?? ""}
              onChange={(e) => onChange({ language: e.target.value || undefined })}
            >
              <option value="">{tr("fpkg.languageAll", undefined, "As the game ships (all languages)")}</option>
              {PLAYGO_LANGUAGES.map(([code, name]) => (
                <option key={code} value={code}>
                  {name}
                </option>
              ))}
            </select>
          </div>
        </>
      )}

      <div className="flex flex-col gap-1">
        <label htmlFor={outputId} className="text-xs text-[var(--color-muted)]">
          {tr("fpkg.output", undefined, "Output folder")}
        </label>
        <div className="flex gap-2">
          <input
            id={outputId}
            data-testid="cq-output"
            className="input min-w-0 flex-1"
            placeholder={tr("fpkg.outputHint", undefined, "Default: ~/Downloads/fpkgs")}
            value={item.outputDir ?? ""}
            disabled={beside}
            onChange={(e) => onChange({ outputDir: e.target.value || undefined })}
          />
          {canBrowse && onPickOutput && (
            <Button
              disabled={beside}
              onClick={() => {
                void onPickOutput().then((picked) => {
                  if (picked) onChange({ outputDir: picked });
                });
              }}
            >
              {tr("fpkg.change", undefined, "Change")}
            </Button>
          )}
        </div>
        {canBeside && (
          <label className="mt-1 flex items-center gap-2">
            <input
              type="checkbox"
              data-testid="cq-output-beside"
              checked={beside}
              onChange={(e) => onChange(changeForBeside(item.source, e.target.checked, rememberedOutput))}
            />
            <span>{tr("fpkg.beside", undefined, "Save each next to its source")}</span>
          </label>
        )}
      </div>

      <FullVerifyOption
        own={{ value: item.fullVerify ?? rememberedVerify, onChange: (fullVerify) => onChange({ fullVerify }) }}
      />

      <div className="flex flex-col gap-1">
        <span className="text-xs text-[var(--color-muted)]">{tr("cq_opt_then", undefined, "Then")}</span>
        <SegmentedControl
          value={thens.includes(item.then) ? item.then : "keep"}
          ariaLabel={tr("cq_opt_then", undefined, "Then")}
          segments={thens.map((t) => ({ value: t, label: thenLabel[t] }))}
          onChange={(t) => {
            const then = t as ConvertThen;
            onChange({ then, host: then !== "keep" ? (item.host ?? host) : null });
          }}
        />
      </div>
      {kind === "image" && item.then !== "keep" && (item.host ?? host) && (
        <ImageDestination
          host={(item.host ?? host)!}
          volume={imageDestFor(item.imageDest, (item.host ?? host)!).volume}
          subpath={imageDestFor(item.imageDest, (item.host ?? host)!).subpath}
          onChange={(volume, subpath) => onChange({ imageDest: { volume, subpath } })}
        />
      )}
      {kind !== "folder" && item.then !== "keep" && (
        <Toggle
          checked={!!item.deleteAfterInstall}
          onChange={(checked) => onChange({ deleteAfterInstall: checked })}
          label={
            kind === "image"
              ? tr("cq_delete_after_image", undefined, "Delete the image once it is on the PS5")
              : tr("cq_delete_after", undefined, "Delete the package once installed")
          }
        />
      )}
    </div>
  );
}
