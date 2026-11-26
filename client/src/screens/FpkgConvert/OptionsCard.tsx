// Card ② Options: where the package goes (remembered), how hard to compress it, and the one
// language it declares when a game needs telling.

import type { ReactNode } from "react";

import type { FpkgCompression, FpkgEstimate, ImageFormat } from "../../api/fpkg";
import { Button, Card } from "../../components";
import { ALL_PLAYGO_LANGUAGES, PLAYGO_LANGUAGES } from "../../lib/playgoLanguages";
import type { MakeKind } from "../../state/convertPrefs";
import type { OutputPlace } from "../../lib/outputPlace";
import { useTr } from "../../state/lang";
import { CompressionTiles } from "./CompressionTiles";
import {
  AmprSection,
  FullVerifyOption,
  ImageChoice,
  PackagePacks,
  UnpackOption,
  type Lz4Choice,
  type UnpackChoice,
} from "./ImageOptions";
import { NextGameNote } from "./MakeCard";
import { prettyBytes } from "./RunCard";

/** The most room a conversion needs on the output drive: the package, which the compressed
 *  image is written straight into, plus a little headroom. */
export function spaceNeeded(plannedSize: number): number {
  return Math.ceil(plannedSize * 1.02);
}

export interface OptionsCardProps {
  outputDir: string;
  onChangeOutput: () => void;
  onOutputTyped: (value: string) => void;
  /** In the Output folder, or next to each game's source; absent hides the choice. */
  outputPlace?: OutputPlace;
  onOutputPlace?: (place: OutputPlace) => void;
  canBrowse: boolean;
  compression: FpkgCompression;
  onCompression: (c: FpkgCompression) => void;
  estimates: Partial<Record<FpkgCompression, FpkgEstimate>> | "pending" | null | undefined;
  locked: boolean;
  /** The package's planned (uncompressed) size, once the game is checked. */
  plannedSize?: number;
  /** The game's size, when it is being made into a compressed image: the warning for that. */
  compressedFrom?: number;
  /** Room on the output drive; absent where the platform does not say. */
  outputFree?: number | null;
  /** The language the package declares to PlayGo: "" keeps the game's own, "all" declares
   *  every one. */
  language?: string;
  /** What Convert makes. Compression and language are a package's: an image carries the
   *  folder's files as they are, and its own compression is chosen beside its format. */
  make?: MakeKind;
  /** The image's options, shown in place of the package's when `make` is "image". */
  image?: {
    format: ImageFormat;
    compress: boolean;
    onChoice: (format: ImageFormat, compress: boolean) => void;
    lz4?: Lz4Choice;
  };
  /** The game is in asset packs and no pack option is on: offer to write plain files again
   *  (an image, a folder, or a package, which also offers to leave the AMPR emulator out). */
  unpack?: UnpackChoice;
  /** The game under ① has been checked (and is one whose files could be looked at: not an
   *  archive). A package then says whether the game is in asset packs; until then, that
   *  checking it shows the options for one that is. */
  gameChecked?: boolean;
  onLanguage?: (code: string) => void;
  /** A game is being made: this card sets what the next game added uses (#669). */
  forNext?: boolean;
  /** The checked game is a PS4 game: a package of it is a PS4 package. */
  ps4?: boolean;
  /** More choices, after the rest. */
  children?: ReactNode;
}

export function OptionsCard(props: OptionsCardProps) {
  const tr = useTr();
  const beside = props.outputPlace === "beside";
  // A PS4 package has neither the PS5 package's compression nor its language; it is always
  // read back whole.
  const ps4Package = props.ps4 === true && props.make !== "image" && props.make !== "folder";
  // A PS5 package is what is being made.
  const pkg = props.make !== "image" && props.make !== "folder" && !ps4Package;
  return (
    <Card padded={false} className="p-3.5 sm:p-4">
      <div className="flex flex-col gap-2.5 text-sm">
        <div className="font-medium">{tr("fpkg.card.options_v2", undefined, "③ Options")}</div>
        {props.forNext && <NextGameNote />}
        {/* A package's asset-pack choices first: what they decide (how large the package is, and
            whether it needs the AMPR emulator) matters more than anything below, and under the
            output folder and four compression tiles they were a long way down on a phone. */}
        {pkg && <PackagePacks unpack={props.unpack} state={props.gameChecked ? "none" : "pending"} />}
        <div className="flex flex-col gap-1">
          <label htmlFor="fpkg-output" className="text-[var(--color-muted)]">
            {tr("fpkg.output", undefined, "Output folder")}
          </label>
          <div className="flex gap-2">
            <input
              id="fpkg-output"
              className="input min-w-0 flex-1"
              placeholder={tr("fpkg.outputHint", undefined, "Default: ~/Downloads/fpkgs")}
              value={props.outputDir}
              disabled={props.locked}
              onChange={(e) => props.onOutputTyped(e.target.value)}
            />
            {props.canBrowse && (
              <Button onClick={props.onChangeOutput} disabled={props.locked}>
                {tr("fpkg.change", undefined, "Change")}
              </Button>
            )}
          </div>
          {props.onOutputPlace && (
            <label className="mt-1 flex items-center gap-2">
              <input
                type="checkbox"
                data-testid="output-beside"
                checked={beside}
                disabled={props.locked}
                onChange={(e) => props.onOutputPlace?.(e.target.checked ? "beside" : "folder")}
              />
              <span>{tr("fpkg.beside", undefined, "Save each next to its source")}</span>
            </label>
          )}
          {props.onOutputPlace && beside && (
            <div className="text-xs text-[var(--color-muted)]" data-testid="output-beside-hint">
              {tr(
                "fpkg.besideHint",
                undefined,
                "Each one goes in the folder that holds its game or image, not inside a game folder. If the name is taken there, (2) is added. Games on a server or the PS5 still go to the Output folder.",
              )}
            </div>
          )}
        </div>
        {props.outputFree != null && (
          <div className="text-xs text-[var(--color-muted)]">
            {tr("fpkg.free", undefined, "Free")}: {prettyBytes(props.outputFree)}
          </div>
        )}
        {props.outputFree != null &&
          props.compressedFrom != null &&
          props.outputFree < props.compressedFrom * 0.4 && (
            <div className="text-xs text-[var(--color-warn)]" data-testid="low-space-compressed">
              {tr(
                "fpkg.lowSpaceCompressed",
                {
                  size: prettyBytes(props.compressedFrom),
                  free: prettyBytes(props.outputFree),
                },
                "Low on free space: this game is {size}, a compressed image is usually about half that, and the output drive has {free}. It may not fit.",
              )}
            </div>
          )}
        {props.outputFree != null &&
          props.plannedSize != null &&
          props.outputFree < spaceNeeded(props.plannedSize) && (
            <div className="text-xs text-[var(--color-warn)]">
              {tr(
                "fpkg.lowSpace",
                {
                  need: prettyBytes(spaceNeeded(props.plannedSize)),
                  free: prettyBytes(props.outputFree),
                },
                "Low on free space: converting can need up to {need} on the output drive, which has {free}.",
              )}
            </div>
          )}
        {props.make === "image" ? (
          <div className="flex flex-col gap-1" data-testid="options-image">
            <div className="text-[var(--color-muted)]">{tr("fpkg.options.image", undefined, "Image")}</div>
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.image.what",
                undefined,
                "One file you copy to a PS5 drive, which ShadowMount+ mounts and shows on the home screen. Nothing is installed.",
              )}
            </div>
            {props.image && (
              <ImageChoice
                format={props.image.format}
                compress={props.image.compress}
                onChoice={props.image.onChoice}
                lz4={props.image.lz4}
                packsNote
              />
            )}
            {props.unpack && <UnpackOption unpack={props.unpack} />}
          </div>
        ) : props.make === "folder" ? (
          <div className="flex flex-col gap-2" data-testid="options-folder">
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.folder.what",
                undefined,
                "The folder is named after the image or package, in the output folder. Nothing is compressed, and nothing reaches the PS5.",
              )}
            </div>
            {/* A PS5 game can have its folder in packs, as an image: LZ4 for a game that reads
                through AMPR, zstd for any PS5 game. */}
            <AmprSection lz4={props.image?.lz4} folder packsNote />
            {props.unpack && <UnpackOption unpack={props.unpack} />}
          </div>
        ) : ps4Package ? (
          <div className="text-xs text-[var(--color-muted)]" data-testid="options-ps4-pkg">
            {tr(
              "fpkg.ps4_pkg.options",
              undefined,
              "A PS4 package keeps the game's files as they are: nothing is compressed, the game keeps all its languages, and every file is read back and compared before the package is kept.",
            )}
          </div>
        ) : (
          <div className="flex flex-col gap-1">
            <div className="text-[var(--color-muted)]">
              {tr("fpkg.compression", undefined, "Compression")}
            </div>
            <CompressionTiles
              value={props.compression}
              onChange={props.onCompression}
              estimates={props.estimates}
              disabled={props.locked}
            />
          </div>
        )}
        {props.onLanguage && pkg && (
          <div className="flex flex-col gap-1">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <label htmlFor="fpkg-language" className="text-[var(--color-muted)]">
              {tr("fpkg.language", undefined, "Game language")}
            </label>
            <select
              id="fpkg-language"
              className="select w-auto!"
              value={props.language ?? ""}
              disabled={props.locked}
              onChange={(e) => props.onLanguage?.(e.target.value)}
            >
              <option value="">{tr("fpkg.languageOwn", undefined, "The game's own languages")}</option>
              <option value={ALL_PLAYGO_LANGUAGES}>{tr("fpkg.languageEvery", undefined, "Every language")}</option>
              {PLAYGO_LANGUAGES.map(([code, name]) => (
                <option key={code} value={code}>
                  {name}
                </option>
              ))}
            </select>
            </div>
            {props.language === ALL_PLAYGO_LANGUAGES && (
              <div className="text-xs text-[var(--color-warn)]" data-testid="language-every-hint">
                {tr(
                  "fpkg.languageEveryHint",
                  undefined,
                  "The package then declares all 31 languages, and the PS5 may list a language pack for each one when you manage the game's content.",
                )}
              </div>
            )}
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.languageHint",
                undefined,
                "Most games follow the console's language. Pick one only for a game that stays in English on a console set to another language: the package then tells the game that language is the one installed. The game has to include that language.",
              )}
            </div>
          </div>
        )}
        {!ps4Package && <FullVerifyOption disabled={props.locked} />}
        {props.children}
      </div>
    </Card>
  );
}
