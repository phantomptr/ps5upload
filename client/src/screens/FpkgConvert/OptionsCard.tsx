// Card ② Options: where the package goes (remembered), how hard to compress it, and the one
// language it declares when a game needs telling.

import type { FpkgCompression, FpkgEstimate } from "../../api/fpkg";
import { Button, Card } from "../../components";
import { PLAYGO_LANGUAGES } from "../../lib/playgoLanguages";
import { useTr } from "../../state/lang";
import { CompressionTiles } from "./CompressionTiles";
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
  canBrowse: boolean;
  compression: FpkgCompression;
  onCompression: (c: FpkgCompression) => void;
  estimates: Record<FpkgCompression, FpkgEstimate> | "pending" | null | undefined;
  locked: boolean;
  /** The package's planned (uncompressed) size, once the game is checked. */
  plannedSize?: number;
  /** Room on the output drive; absent where the platform does not say. */
  outputFree?: number | null;
  /** The one language the package declares to PlayGo; "" keeps every language. */
  language?: string;
  onLanguage?: (code: string) => void;
}

export function OptionsCard(props: OptionsCardProps) {
  const tr = useTr();
  return (
    <Card>
      <div className="flex flex-col gap-3 text-sm">
        <div className="font-medium">{tr("fpkg.card.options", undefined, "② Options")}</div>
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
        </div>
        {props.outputFree != null && (
          <div className="text-xs text-[var(--color-muted)]">
            {tr("fpkg.free", undefined, "Free")}: {prettyBytes(props.outputFree)}
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
        {props.onLanguage && (
          <div className="flex flex-col gap-1">
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
              <option value="">{tr("fpkg.languageAll", undefined, "As the game ships (all languages)")}</option>
              {PLAYGO_LANGUAGES.map(([code, name]) => (
                <option key={code} value={code}>
                  {name}
                </option>
              ))}
            </select>
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.languageHint",
                undefined,
                "Most games follow the console's language. Pick one only for a game that stays in English on a console set to another language: the package then tells the game that language is the one installed. The game has to include that language.",
              )}
            </div>
          </div>
        )}
      </div>
    </Card>
  );
}
