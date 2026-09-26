// Card ① Game: where the game comes from (drop, pick or type), and what the check found.

import { FolderInput } from "lucide-react";

import type { FpkgInspection } from "../../api/fpkg";
import { Card, Input } from "../../components";
import { BrowseButton, PathLabel } from "../../components/BrowseButton";
import { isRemotePath } from "../../lib/remotePath";
import { useTr } from "../../state/lang";
import { prettyBytes } from "./RunCard";

type Tr = (key: string, vars?: Record<string, string | number>, fallback?: string) => string;

/** "05.10" → "5.10". */
function shortFw(v: string): string {
  const [a, b] = v.split(".");
  return `${Number.parseInt(a ?? "0", 10)}.${b ?? "00"}`;
}

/** What firmware the package will ask for: the backport's own when the modules allow lower
 *  than the game declares, else the declared one; null when the game names none. */
export function firmwareLine(inspection: FpkgInspection, tr: Tr): string | null {
  if (inspection.min_firmware) {
    return tr(
      "fpkg.runsOnBackport",
      { fw: shortFw(inspection.min_firmware) },
      "Runs on FW {fw} and later (backported)",
    );
  }
  if (inspection.required_firmware) {
    return tr("fpkg.runsOn", { fw: shortFw(inspection.required_firmware) }, "Runs on FW {fw} and later");
  }
  return null;
}

/** "UP9000-PPSA03016_00-…" → "PPSA03016". */
export function titleIdOf(contentId: string | null | undefined): string | null {
  return contentId && contentId.length >= 16 ? contentId.slice(7, 16) : null;
}

export interface GameCardProps {
  source: string;
  onSourceTyped: (value: string) => void;
  onCheck: () => void;
  onBrowseFolder: () => void;
  onBrowseImage: () => void;
  /** A folder or image picked on a saved server. */
  onRemotePick: (path: string) => void;
  canBrowse: boolean;
  inspection: FpkgInspection | null;
  checking: boolean;
  /** A run is going: nothing here may change. */
  locked: boolean;
  dropActive: boolean;
}

export function GameCard(props: GameCardProps) {
  const tr = useTr();
  const { inspection } = props;
  const warnings = inspection?.checks.filter((c) => !c.ok) ?? [];
  const passed = inspection?.checks.filter((c) => c.ok) ?? [];
  const fw = inspection ? firmwareLine(inspection, tr) : null;
  return (
    <Card>
      <div className="flex flex-col gap-2">
        <div className="text-sm font-medium">{tr("fpkg.card.game", undefined, "① Game")}</div>
        <div
          className={`flex flex-col items-center gap-2 rounded-lg border-2 border-dashed p-4 text-center text-sm ${
            props.dropActive
              ? "border-[var(--color-accent)] bg-[var(--color-accent-soft,transparent)]"
              : "border-[var(--color-border)]"
          } ${props.locked ? "opacity-60" : ""}`}
        >
          <FolderInput size={20} aria-hidden className="text-[var(--color-muted)]" />
          <div>
            {tr("fpkg.dropHere", undefined, "Drop a game folder, .exfat or .ffpkg here")}
          </div>
          {props.canBrowse && (
            <div className="flex flex-wrap justify-center gap-2">
              <BrowseButton
                mode="folder"
                remote
                label={tr("fpkg.browseFolder", undefined, "Folder…")}
                title={tr("fpkg.pickFolder", undefined, "Choose the game folder")}
                disabled={props.locked || props.checking}
                onMainClick={props.onBrowseFolder}
                onPick={props.onRemotePick}
              />
              <BrowseButton
                mode="file"
                remote
                label={tr("fpkg.browseImage", undefined, "Image…")}
                title={tr("fpkg.pickImage", undefined, "Choose an .exfat or .ffpkg image")}
                filters={[{ name: "Game image", extensions: ["exfat", "ffpkg"] }]}
                disabled={props.locked || props.checking}
                onMainClick={props.onBrowseImage}
                onPick={props.onRemotePick}
              />
            </div>
          )}
        </div>
        <div className="flex gap-2">
          <div className="flex-1">
            <Input
              id="fpkg-source"
              label={tr("fpkg.source", undefined, "Game source")}
              placeholder={tr(
                "fpkg.sourcePlaceholder",
                undefined,
                "/games/PPSA09519.exfat, /games/my-game, /games/game.ffpkg",
              )}
              value={props.source}
              disabled={props.locked}
              onChange={(e) => props.onSourceTyped(e.target.value)}
              onBlur={props.onCheck}
            />
          </div>
        </div>
        {isRemotePath(props.source) && !props.locked && (
          <div className="flex items-center gap-1 text-sm text-[var(--color-muted)]">
            <PathLabel path={props.source} />
            <span>
              {"— "}
              {tr(
                "fpkg.remoteSource",
                undefined,
                "on a server. It is copied to this computer when you start.",
              )}
            </span>
          </div>
        )}
        {props.checking && (
          <div className="text-sm text-[var(--color-muted)]">
            {tr("fpkg.checking", undefined, "Checking…")}
          </div>
        )}
        {inspection && !props.checking && (
          <div className="flex flex-col gap-1 text-sm">
            <div>
              <span className={warnings.length ? "" : "text-[var(--color-good)]"}>
                {warnings.length ? "⚠" : "✓"}{" "}
              </span>
              <span className="font-medium">
                {inspection.title ?? inspection.content_id ?? inspection.source}
              </span>
              {titleIdOf(inspection.content_id) && <span> · {titleIdOf(inspection.content_id)}</span>}
              <span>
                {" · "}
                {prettyBytes(inspection.bytes)} ·{" "}
                {tr("fpkg.fileCount", { count: inspection.files }, "{count} files")}
              </span>
            </div>
            <div className="text-[var(--color-muted)]">
              {fw}
              {fw ? " · " : ""}
              {tr("fpkg.checksPassed", { count: passed.length }, "{count} checks passed")}
            </div>
            {warnings.map((c) => (
              <div key={c.name} className="text-[var(--color-warn)]">
                {c.name}: {c.detail}
              </div>
            ))}
            {passed.length > 0 && (
              <details>
                <summary className="cursor-pointer text-[var(--color-muted)]">
                  {tr("fpkg.showChecks", undefined, "Show checks")}
                </summary>
                <ul className="mt-1 flex flex-col gap-0.5 text-[var(--color-muted)]">
                  {passed.map((c) => (
                    <li key={c.name}>
                      {c.name} — {c.detail}
                    </li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        )}
      </div>
    </Card>
  );
}
