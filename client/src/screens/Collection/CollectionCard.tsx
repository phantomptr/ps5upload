import { memo } from "react";
import { useNavigate } from "react-router";
import {
  Check,
  CircleDot,
  Copy as CopyIcon,
  Download,
  ExternalLink,
  PackageOpen,
  Shield,
  ShieldCheck,
  Shrink,
  Wrench,
} from "lucide-react";

import type { CollectionGame, GameConsoleState } from "../../api/collection";
import { addOnCount, formatCollectionBytes } from "../../lib/collectionView";
import { useTr } from "../../state/lang";
import { COVER_THUMB_PX, CollectionCover } from "./CollectionCover";
import {
  CoverCaption,
  CoverFrame,
  CoverTag,
  MetaDot,
  PlatformTag,
} from "../../components/GameIconFrame";
import type { CopyActivity } from "../../state/copyActivity";
import {
  OverflowMenu,
  type OverflowMenuItem,
} from "../../components/OverflowMenu";
import { bestConvertible } from "../../lib/collectionConvert";
import {
  firmwareShortfall,
  shrinkCandidate,
} from "../../lib/collectionInsights";
import { openInConvert } from "../../lib/collectionActions";
import { installOffers } from "../GameHub/collectionInstall";
import { pushNotification } from "../../state/notifications";
import { startCopyChecks } from "../../state/copyChecks";
import { gamePath } from "../../lib/gamePage";
import { formatPlayTime } from "../../state/playTime";
import { GameIcon } from "../../components/GameIcon";
import { PlayControl, type PlayState } from "./PlayControl";

/** What the quick actions and badges know about the selected console. */
export interface GameConsoleContext {
  /** The connected console, when its helper answers. */
  host?: string;
  /** Its firmware ("9.60"), when known. */
  consoleFw?: string | null;
  /** What it has of this game. */
  consoleState?: GameConsoleState;
  /** The cheat index has cheats for this game. */
  hasCheats?: boolean;
}

/** A game's quick actions, for its card and table row: open its page, install what the console
 *  lacks, convert or shrink its best copy, its cheats, backport it, check its copies. */
export function GameQuickActions({
  game,
  onOpen,
  host,
  consoleFw,
  consoleState,
  hasCheats,
}: {
  game: Pick<CollectionGame, "game_id" | "title" | "locations" | "platform">;
  onOpen: (gameId: string) => void;
} & GameConsoleContext) {
  const tr = useTr();
  const navigate = useNavigate();
  const convertible = bestConvertible(game.locations);
  const shrink = shrinkCandidate(game);
  const installed = !!consoleState?.installed;
  const tooNew = firmwareShortfall(game, consoleFw ?? null);
  const queue = (offers: NonNullable<GameConsoleState["update"]>[]) => {
    if (!host) return;
    const n = installOffers(
      host,
      offers.map((offer) => ({ game, offer })),
    );
    pushNotification(
      "info",
      tr("collection.queued", { n }, "{n} packages queued for install"),
      { link: "/send" },
    );
  };
  const items: OverflowMenuItem[] = [
    {
      icon: <ExternalLink size={12} />,
      label: tr("game_open_page", undefined, "Open game page"),
      onSelect: () => onOpen(game.game_id),
    },
  ];
  if (host && installed && consoleState?.update) {
    const update = consoleState.update;
    items.push({
      icon: <Download size={12} />,
      label: tr(
        "collection.install_update",
        { v: update.version },
        "Install update {v}",
      ),
      title: tr(
        "collection.badge_update_hint",
        { have: consoleState.installed_version ?? "?", v: update.version },
        "The console has {have}; the collection has update {v}.",
      ),
      onSelect: () => queue([update]),
    });
  }
  if (
    host &&
    installed &&
    consoleState &&
    consoleState.dlc_missing.length > 0
  ) {
    const dlc = consoleState.dlc_missing;
    items.push({
      icon: <Download size={12} />,
      label: tr(
        "collection.install_dlc",
        { n: dlc.length },
        "Install missing DLC ({n})",
      ),
      onSelect: () => queue(dlc),
    });
  }
  if (convertible) {
    items.push({
      icon: <PackageOpen size={12} />,
      label: tr("collection.convert", undefined, "Convert…"),
      title: tr(
        "collection.convert_hint_v2",
        undefined,
        "Open it in Workshop: make an installable package, or a different game image.",
      ),
      onSelect: () => openInConvert(navigate, [convertible]),
    });
  }
  if (shrink) {
    items.push({
      icon: <Shrink size={12} />,
      label: tr("collection.shrink", undefined, "Make it smaller…"),
      title: tr(
        "collection.shrink_hint_v2",
        undefined,
        "Open it in Workshop set to a compressed game image (.ffpfsc). How much room it saves depends on the game.",
      ),
      onSelect: () => openInConvert(navigate, [shrink], { compress: true }),
    });
  }
  if (hasCheats || installed) {
    items.push({
      icon: <Shield size={12} />,
      label: tr("collection.cheats", undefined, "Cheats…"),
      onSelect: () => navigate("/cheats", { state: { titleId: game.game_id } }),
    });
  }
  if (installed && tooNew) {
    items.push({
      icon: <Wrench size={12} />,
      label: tr("collection.backport", undefined, "Backport…"),
      title: tr(
        "collection.backport_hint",
        { need: tooNew, have: consoleFw ?? "?" },
        "It asks for firmware {need} and this PS5 is on {have}. Backporting can let it start.",
      ),
      onSelect: () =>
        navigate(gamePath(game.game_id), { state: { backport: true } }),
    });
  }
  if (game.locations.length > 0) {
    items.push({
      icon: <ShieldCheck size={12} />,
      label:
        game.locations.length === 1
          ? tr("collection.check_copy", undefined, "Check the copy")
          : tr(
              "collection.check_copies",
              { n: game.locations.length },
              "Check its {n} copies",
            ),
      title: tr(
        "collection.check_hint",
        undefined,
        "Read every copy back to find one that is damaged or cut short. Runs on this computer, one copy at a time.",
      ),
      onSelect: () => {
        void startCopyChecks(game.locations.map((l) => l.absolute_path));
        pushNotification(
          "info",
          tr(
            "collection.check_started",
            { title: game.title },
            "Checking {title}",
          ),
          { link: gamePath(game.game_id) },
        );
      },
    });
  }
  return (
    <OverflowMenu
      items={items}
      ariaLabel={`${tr("installed_more_actions", undefined, "More actions")}: ${game.title}`}
      buttonTitle={tr("installed_more_actions", undefined, "More actions")}
    />
  );
}

/** "1 copy · 2 add-ons": copies are full copies of the game, add-ons its updates and DLC. */
export function useCopiesLabel() {
  const tr = useTr();
  return (g: CollectionGame) => {
    const copies =
      g.copies === 1
        ? tr("collection.copies_one", undefined, "1 copy")
        : tr("collection.copies_n", { n: g.copies }, "{n} copies");
    const addOns = addOnCount(g);
    if (addOns === 0) return copies;
    const extra =
      addOns === 1
        ? tr("collection.addons_one", undefined, "1 add-on")
        : tr("collection.addons_n", { n: addOns }, "{n} add-ons");
    return `${copies} · ${extra}`;
  };
}

/** What the selected console has of this game, as a short badge; nothing when it lacks it. */
export function ConsoleBadge({ state }: { state?: GameConsoleState }) {
  const tr = useTr();
  if (!state?.installed) return null;
  if (state.update) {
    return (
      <CoverTag
        tone="accent"
        title={tr(
          "collection.badge_update_hint",
          { have: state.installed_version ?? "?", v: state.update.version },
          "The console has {have}; the collection has update {v}.",
        )}
      >
        {tr(
          "collection.badge_update",
          { v: state.update.version },
          "Update {v}",
        )}
      </CoverTag>
    );
  }
  return (
    <CoverTag
      tone="good"
      title={
        state.dlc_missing.length > 0
          ? tr(
              "collection.badge_dlc_hint",
              { n: state.dlc_missing.length },
              "This computer has {n} DLC for it that this PS5 has not installed.",
            )
          : undefined
      }
    >
      {state.installed_version
        ? tr(
            "collection.badge_installed_v",
            { v: state.installed_version },
            "Installed · {v}",
          )
        : tr("collection.badge_installed", undefined, "Installed")}
      {state.dlc_missing.length > 0
        ? ` · +${state.dlc_missing.length} DLC`
        : ""}
    </CoverTag>
  );
}

/** "Needs FW 10.00": the game asks for newer firmware than the selected console has. */
export function FirmwareBadge({ need, have }: { need: string; have: string }) {
  const tr = useTr();
  return (
    <CoverTag
      tone="warn"
      title={tr(
        "collection.fw_hint",
        { need, have },
        "It asks for firmware {need}; this PS5 is on {have}. It may not start unless it is backported.",
      )}
    >
      {tr("collection.fw_badge", { need }, "Needs FW {need}")}
    </CoverTag>
  );
}

/** "Sending 34%", "Installing", "Queued", "Building": what is happening to the game right now. */
function ActivityBadge({ activity }: { activity: CopyActivity }) {
  const tr = useTr();
  const pct =
    "pct" in activity && activity.pct !== null ? ` ${activity.pct}%` : "";
  const label =
    activity.phase === "building"
      ? tr("collection.badge_building", undefined, "Building") + pct
      : activity.phase === "queued"
        ? tr("collection.badge_queued", undefined, "Queued")
        : activity.phase === "sending" && activity.installing
          ? tr("collection.badge_installing", undefined, "Installing") + pct
          : tr("collection.badge_sending", undefined, "Sending") + pct;
  return <CoverTag tone="accent">{label}</CoverTag>;
}

// Memoised: a big collection is hundreds of cards, and the screen re-renders on
// every keystroke in the search box.
export const CollectionCard = memo(function CollectionCard({
  game,
  onOpen,
  consoleState,
  activity,
  host,
  consoleFw,
  hasCheats,
  playSec,
  selecting,
  selected,
  onToggleSelect,
  onlyOnConsole,
  onlyOnDrives,
  drivePath,
  running,
  play,
  onPlay,
  onStop,
}: {
  game: CollectionGame;
  onOpen: (gameId: string) => void;
  activity?: CopyActivity | null;
  /** Seconds seen played on the selected console. */
  playSec?: number;
  /** Select mode: the card picks instead of opening. */
  selecting?: boolean;
  selected?: boolean;
  onToggleSelect?: (gameId: string) => void;
  /** Installed on the selected console with no copy here: says so instead of "0 copies". */
  onlyOnConsole?: boolean;
  /** Only on the selected console's drives (a folder or image it has not registered). */
  onlyOnDrives?: boolean;
  /** That game's folder on the console, for its icon. */
  drivePath?: string;
  /** Running on the selected console now. */
  running?: boolean;
  /** Play / Close game, when it is installed on the selected console and can start there. */
  play?: PlayState;
  onPlay?: (gameId: string) => void;
  onStop?: (gameId: string) => void;
} & GameConsoleContext) {
  const tr = useTr();
  const copiesLabel = useCopiesLabel();
  // The cover and the caption each open the game; the "⋯" menu beside the caption holds the
  // quick actions (a button cannot hold another, so the card is not one button any more).
  const open = () =>
    selecting ? onToggleSelect?.(game.game_id) : onOpen(game.game_id);
  const tooNew = firmwareShortfall(game, consoleFw ?? null);
  const showPlay = !!play && !selecting && !!onPlay && !!onStop;
  const focusRing =
    "rounded-[var(--radius-card)] text-start focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-[var(--color-accent)]";
  return (
    <div className="group flex min-w-0 flex-col" data-testid="collection-card">
      <div className="relative flex min-w-0 flex-col">
        <button
          type="button"
          onClick={open}
          className={`${focusRing} ${selected ? "outline-2 outline-offset-4 outline-[var(--color-accent)]" : ""}`}
          aria-label={game.title}
          aria-pressed={selecting ? !!selected : undefined}
        >
          <CoverFrame
            interactive
            overlay={
              <>
                <div className="absolute left-2 top-2 flex items-center gap-1">
                  {selecting && (
                    <span
                      aria-hidden
                      className={`grid size-5 place-items-center rounded-full border ${
                        selected
                          ? "border-[var(--color-accent)] bg-[var(--color-accent)] text-white"
                          : "border-white/70 bg-black/40"
                      }`}
                    >
                      {selected && <Check size={12} strokeWidth={3} />}
                    </span>
                  )}
                  <PlatformTag platform={game.platform} />
                  {hasCheats && (
                    <CoverTag
                      tone="neutral"
                      icon={<Shield size={11} aria-hidden />}
                      title={tr(
                        "collection.cheats_hint",
                        undefined,
                        "The cheat repositories have cheats for this game.",
                      )}
                    >
                      {tr("collection.cheats_tag", undefined, "Cheats")}
                    </CoverTag>
                  )}
                </div>
                {game.is_duplicate && (
                  <span className="absolute right-2 top-2">
                    <CoverTag
                      tone="warn"
                      icon={<CopyIcon size={11} aria-hidden />}
                      title={tr(
                        "collection.duplicate_hint",
                        undefined,
                        "More than one full copy of this game is on disk.",
                      )}
                    >
                      {tr("collection.duplicate", undefined, "Duplicate")}
                    </CoverTag>
                  </span>
                )}
                {/* Room on the right for the Play control, which sits over this corner. */}
                <div
                  className={`absolute inset-x-2 bottom-2 flex flex-wrap items-end justify-between gap-1 ${
                    showPlay ? "pe-11 sm:pe-[6.75rem]" : ""
                  }`}
                >
                  {running ? (
                    <CoverTag
                      tone="good"
                      icon={
                        <CircleDot
                          size={11}
                          className="animate-pulse"
                          aria-hidden
                        />
                      }
                      title={tr(
                        "installed_now_playing",
                        undefined,
                        "Now playing",
                      )}
                    >
                      {tr("installed_badge_playing", undefined, "Playing")}
                    </CoverTag>
                  ) : (
                    <ConsoleBadge state={consoleState} />
                  )}
                  {tooNew && consoleFw && (
                    <FirmwareBadge need={tooNew} have={consoleFw} />
                  )}
                  {activity &&
                    (activity.phase === "building" ||
                      activity.phase === "queued" ||
                      activity.phase === "sending") && (
                      <ActivityBadge activity={activity} />
                    )}
                </div>
              </>
            }
          >
            {(onlyOnConsole || onlyOnDrives) && host ? (
              // Nothing of it on this computer to read a cover from: the console's own icon.
              <GameIcon
                host={host}
                titleId={game.game_id}
                gamePath={drivePath}
                alt=""
                rounded=""
                fill
              />
            ) : (
              <CollectionCover game={game} size={COVER_THUMB_PX} className="h-full" />
            )}
          </CoverFrame>
        </button>
        {showPlay && (
          // Beside the cover's button, not in it (a button cannot hold another): over the cover's
          // bottom corner, lifted with the cover on hover.
          <div className="absolute bottom-[0.9375rem] end-[0.9375rem] flex transition-transform duration-200 ease-out group-hover:-translate-y-0.5 motion-reduce:transition-none motion-reduce:group-hover:translate-y-0 sm:bottom-[1.0625rem] sm:end-[1.0625rem]">
            <PlayControl
              state={play}
              title={game.title}
              onPlay={() => onPlay(game.game_id)}
              onStop={() => onStop(game.game_id)}
            />
          </div>
        )}
      </div>
      <div className="flex min-w-0 items-start gap-1">
        <button
          type="button"
          onClick={open}
          className={`min-w-0 flex-1 ${focusRing}`}
          tabIndex={-1}
        >
          <CoverCaption
            title={game.title}
            titleAttr={`${game.title} · ${game.game_id}`}
            meta={
              <>
                {onlyOnConsole ? (
                  <span className="truncate">
                    {tr(
                      "collection.only_on_console",
                      undefined,
                      "On the PS5 only",
                    )}
                  </span>
                ) : onlyOnDrives ? (
                  <span className="truncate">
                    {tr(
                      "collection.only_on_drives",
                      undefined,
                      "On the PS5's drives",
                    )}
                  </span>
                ) : (
                  <>
                    <span className="truncate">{copiesLabel(game)}</span>
                    <MetaDot />
                    <span className="shrink-0 tabular-nums">
                      {formatCollectionBytes(game.total_size_bytes)}
                    </span>
                  </>
                )}
                {!!playSec && playSec > 0 && (
                  <>
                    <MetaDot />
                    <span
                      className="shrink-0 tabular-nums"
                      title={tr(
                        "collection.played_hint",
                        undefined,
                        "Time this app has seen it played on this PS5",
                      )}
                    >
                      {formatPlayTime(playSec)}
                    </span>
                  </>
                )}
              </>
            }
          />
        </button>
        {!selecting && (
          <div className="shrink-0 pt-1.5">
            <GameQuickActions
              game={game}
              onOpen={onOpen}
              host={host}
              consoleFw={consoleFw}
              consoleState={consoleState}
              hasCheats={hasCheats}
            />
          </div>
        )}
      </div>
    </div>
  );
});
