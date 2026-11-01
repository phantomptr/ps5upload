// Card ① Game: the one place games are added (drop, pick or type; one is checked here, several
// go into the queue), and what the check found.

import type { ReactNode } from "react";
import { FolderInput, Gamepad2 } from "lucide-react";

import type { FpkgInspection } from "../../api/fpkg";
import { Button, Card, Input } from "../../components";
import { AddSources, GAME_FILE_FILTERS } from "../../components/AddSources";
import { PathLabel } from "../../components/BrowseButton";
import { isRemotePath } from "../../lib/remotePath";
import { isArchiveSource } from "../../state/fpkgConversion";
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
    const fw = shortFw(inspection.min_firmware);
    // Backports stamp their modules with the FW 4.00 pair. Modules a little below the
    // declared firmware are just an older SDK than the game asks for, not a backport.
    if (Number.parseInt(fw, 10) <= 4) {
      return tr("fpkg.runsOnBackport", { fw }, "Runs on FW {fw} and later (backported)");
    }
    return tr(
      "fpkg.runsOnModules",
      { fw, declared: shortFw(inspection.required_firmware ?? inspection.min_firmware) },
      "Runs on FW {fw} and later (its modules allow it; it declares {declared})",
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
  /** Games picked on this computer: one becomes the game here, several go into the queue. */
  onAdd: (sources: { path: string; isDir: boolean }[]) => void;
  /** A folder that holds games: its games go into the queue. */
  onScanFolder: (folder: string) => void;
  /** A folder or image picked on a saved server. */
  onRemotePick: (path: string, isDir: boolean) => void;
  /** Picking could not open (not a cancel): the screen shows why. */
  onPickError?: (message: string) => void;
  /** Open the chosen game in the package viewer; absent when there's nothing to show. */
  onView?: () => void;
  /** Browse the connected console for a game; absent when no console answers. */
  onBrowseConsole?: () => void;
  canBrowse: boolean;
  inspection: FpkgInspection | null;
  checking: boolean;
  /** A run is going: the game checked here may not change; games picked go into the queue. */
  locked: boolean;
  /** While locked: games picked now are made after the run without pressing Start (#621). */
  queueStarts?: boolean;
  dropActive: boolean;
  /** A .rar's password, when the source is one; never stored. */
  password: string;
  onPassword: (value: string) => void;
  /** What else can be done with this source (Workshop's Look Inside, Open as a Drive). */
  tools?: ReactNode;
}

export function GameCard(props: GameCardProps) {
  const tr = useTr();
  const { inspection } = props;
  const warnings = inspection?.checks.filter((c) => !c.ok) ?? [];
  const passed = inspection?.checks.filter((c) => c.ok) ?? [];
  const fw = inspection ? firmwareLine(inspection, tr) : null;
  return (
    <Card padded={false} className="p-3.5 sm:p-4">
      <div className="flex flex-col gap-2">
        <div className="text-sm font-medium">{tr("fpkg.card.game", undefined, "① Game")}</div>
        {/* A modest drop area (the user found the tall one wasteful): one row of what to do and
            the pick buttons, wrapping on a phone. The whole card's window takes the drop. */}
        <div
          data-testid="game-drop"
          className={`flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-dashed px-3 py-2 text-sm ${
            props.dropActive
              ? "border-[color-mix(in_srgb,var(--color-accent)_40%,transparent)] bg-[var(--color-accent-soft,transparent)]"
              : "border-[var(--color-border)]"
          }`}
        >
          <div className="flex min-w-0 items-center gap-2 text-[var(--color-muted)]">
            <FolderInput size={16} aria-hidden className="shrink-0" />
            <span>{tr("fpkg.dropHere", undefined, "Drop a game folder, image or archive here")}</span>
          </div>
          {props.canBrowse && (
            <>
              <AddSources
                className="justify-start!"
                fileFilters={GAME_FILE_FILTERS}
                disabled={props.checking}
                onFiles={(paths) => props.onAdd(paths.map((path) => ({ path, isDir: false })))}
                onFolders={(paths) => props.onAdd(paths.map((path) => ({ path, isDir: true })))}
                onRemote={props.onRemotePick}
                onScanFolder={props.onScanFolder}
                onError={props.onPickError}
              >
                {props.onBrowseConsole && (
                  <Button
                    variant="secondary"
                    size="sm"
                    leftIcon={<Gamepad2 size={14} />}
                    disabled={props.checking}
                    onClick={props.onBrowseConsole}
                  >
                    {tr("fpkg.browseConsole", undefined, "On the PS5…")}
                  </Button>
                )}
              </AddSources>
              <div className="w-full text-xs text-[var(--color-muted)]" data-testid="add-sources-hint">
                {props.locked
                  ? props.queueStarts
                    ? tr(
                        "workshop_add_locked_v2",
                        undefined,
                        "A game is being made: games you pick or drop now go into the queue and are made after it.",
                      )
                    : tr(
                        "workshop_add_locked_manual",
                        undefined,
                        "A game is being made: games you pick or drop now wait in the queue until you start it.",
                      )
                  : tr(
                      "workshop_add_hint",
                      undefined,
                      "Pick one game to check it here. Several games, or a folder of games, go into the queue and are made one after another.",
                    )}
              </div>
            </>
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
                "/games/PPSA09519.exfat, /games/my-game, /downloads/game.7z",
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
              {isArchiveSource(props.source)
                ? tr(
                    "fpkg.remoteArchive",
                    undefined,
                    "an archive on a server. It is copied here and unpacked when you start, and both are removed when the run ends.",
                  )
                : tr(
                    "fpkg.remoteSource",
                    undefined,
                    "on a server. It is read from there while it converts; nothing is copied to this computer.",
                  )}
            </span>
          </div>
        )}
        {isArchiveSource(props.source) && !isRemotePath(props.source) && !props.locked && (
          <div className="flex items-center gap-1 text-sm text-[var(--color-muted)]">
            <PathLabel path={props.source} />
            <span>
              {"— "}
              {tr(
                "fpkg.archiveSource",
                undefined,
                "an archive. It is unpacked into the output folder when you start, and removed when the run ends.",
              )}
            </span>
          </div>
        )}
        {props.source.startsWith("ps5://") && !props.locked && (
          <div className="flex items-center gap-1 text-sm text-[var(--color-muted)]">
            <span className="truncate font-medium text-[var(--color-text)]">
              {props.source.replace(/^ps5:\/\/[^/]+/, "PS5 ")}
            </span>
            <span>
              {"— "}
              {tr(
                "fpkg.consoleSource",
                undefined,
                "on the PS5. It is read from there through the ps5upload helper while it converts; nothing is copied first.",
              )}
            </span>
          </div>
        )}
        {/\.rar$/i.test(props.source.trim()) && (
          <Input
            id="fpkg-archive-password"
            type="password"
            autoComplete="off"
            label={tr("fpkg.archivePassword", undefined, "Archive password")}
            hint={tr("fpkg.archivePasswordHint", undefined, "Only for a password-protected .rar. It is not saved.")}
            value={props.password}
            disabled={props.locked}
            onChange={(e) => props.onPassword(e.target.value)}
          />
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
              {props.onView && (
                <button
                  type="button"
                  onClick={props.onView}
                  className="ml-2 text-[var(--color-accent)] hover:underline"
                >
                  {tr("viewer_open", undefined, "View details")}
                </button>
              )}
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
        {props.tools}
      </div>
    </Card>
  );
}
