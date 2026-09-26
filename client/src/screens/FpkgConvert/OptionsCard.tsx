// Card ② Options: where the package goes (remembered) and how hard to compress it.

import type { FpkgCompression, FpkgEstimate } from "../../api/fpkg";
import { Button, Card } from "../../components";
import { useTr } from "../../state/lang";
import { CompressionTiles } from "./CompressionTiles";

export interface OptionsCardProps {
  outputDir: string;
  onChangeOutput: () => void;
  onOutputTyped: (value: string) => void;
  canBrowse: boolean;
  compression: FpkgCompression;
  onCompression: (c: FpkgCompression) => void;
  estimates: Record<FpkgCompression, FpkgEstimate> | null | undefined;
  locked: boolean;
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
              className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-transparent px-2 py-1"
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
      </div>
    </Card>
  );
}
