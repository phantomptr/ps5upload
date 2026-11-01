import {
  BarChart3,
  Gamepad2,
  Minimize,
  PictureInPicture2,
  Scaling,
  Settings2,
  Square,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";

import { FIT_ORDER, type PictureFit } from "../../lib/remotePlayStream";
import { MicTile } from "./MicControl";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";
import { CaptureRow } from "./CaptureControls";

/** Volume, on the picture's bar and in the player menu (Settings has it too). Keys and presses
 *  on it never reach the pad. */
export function VolumeSlider({ className }: { className: string }) {
  const tr = useTr();
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const muted = prefs.muted || prefs.volume === 0;
  const pct = muted ? 0 : Math.round(prefs.volume * 100);
  return (
    <input
      type="range"
      min={0}
      max={100}
      step={5}
      value={pct}
      onChange={(e) => {
        const v = Number(e.target.value) / 100;
        setPrefs(v === 0 ? { muted: true } : { volume: v, muted: false });
      }}
      onKeyDown={(e) => e.stopPropagation()}
      onKeyUp={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
      aria-label={tr("rp_volume", undefined, "Volume")}
      title={`${tr("rp_volume", undefined, "Volume")} ${pct}%`}
      className={className}
      data-testid="rp-volume"
    />
  );
}

/** One finger-sized tile: the icon with its name under it. */
function Tile({
  label,
  icon,
  onClick,
  testId,
  active,
  danger,
}: {
  label: string;
  icon: React.ReactNode;
  onClick: () => void;
  testId?: string;
  active?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      aria-pressed={active}
      data-testid={testId}
      className={`flex min-h-16 min-w-0 flex-col items-center justify-center gap-1 rounded-xl px-1 py-2 text-center text-xs font-medium leading-tight text-white transition-colors ${
        danger
          ? "bg-[var(--color-danger,#dc2626)] hover:brightness-110"
          : active
            ? "bg-white/25 hover:bg-white/30"
            : "bg-white/10 hover:bg-white/20"
      }`}
    >
      {icon}
      <span className="line-clamp-2 break-words">{label}</span>
    </button>
  );
}

/** A picture size's short name, for the full-screen button and menu. */
export function fitName(tr: ReturnType<typeof useTr>, f: PictureFit): string {
  if (f === "fill") return tr("rp_fit_short_fill", undefined, "Fill");
  if (f === "stretch") return tr("rp_fit_stretch", undefined, "Stretch");
  return tr("rp_fit_short_fit", undefined, "Fit");
}

/**
 * The player menu on a touch-sized player, as finger-sized tiles. In full screen (behind the
 * small handle) it is the whole menu, Leave full screen and Stop included; under the picture
 * those two are on the bar itself, so the menu holds the rest.
 */
export function PlayMenu({
  inFull,
  pipKind,
  onClose,
  onExitFull,
  onPip,
  onSettings,
  onStop,
  onGame,
}: {
  inFull: boolean;
  pipKind: unknown;
  onClose: () => void;
  onExitFull: () => void;
  onPip: () => void;
  onSettings: () => void;
  onStop: () => void;
  /** The Game panel (the running game, its cheats, the console). */
  onGame: () => void;
}) {
  const tr = useTr();
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const pip = useRemotePlayStore((s) => s.pip);
  const name = useRemotePlayStore((s) => s.target?.name ?? "");
  const muted = prefs.muted || prefs.volume === 0;
  return (
    <div className="space-y-2" data-testid="rp-menu">
      {inFull && (
        <div className="flex items-center gap-2 px-1 pb-1 text-sm font-medium text-white">
          <span className="inline-block h-2 w-2 shrink-0 rounded-full bg-[var(--color-success,#22c55e)]" />
          <span className="min-w-0 flex-1 truncate">{name}</span>
          <CaptureRow icons onDone={onClose} />
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onClose();
            }}
            aria-label={tr("rp_menu_close", undefined, "Close the menu")}
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-white/10 hover:bg-white/20"
            data-testid="rp-menu-close"
          >
            <X size={16} />
          </button>
        </div>
      )}
      <div className="grid grid-cols-3 gap-2">
        {inFull && (
          <Tile
            label={tr("rp_exit_fullscreen", undefined, "Leave full screen")}
            icon={<Minimize size={18} />}
            onClick={onExitFull}
            testId="rp-fullscreen"
            active
          />
        )}
        <Tile
          label={tr("rp_show_stats_short", undefined, "Stats")}
          icon={<BarChart3 size={18} />}
          onClick={() => setPrefs({ showStats: !prefs.showStats })}
          testId="rp-toggle-stats"
          active={prefs.showStats}
        />
        <MicTile />
        {!!pipKind && (
          <Tile
            label={
              pip
                ? tr("rp_pip_exit", undefined, "Leave picture in picture")
                : tr("rp_pip", undefined, "Picture in picture")
            }
            icon={<PictureInPicture2 size={18} />}
            onClick={onPip}
            testId="rp-pip"
            active={!!pip}
          />
        )}
        <Tile
          label={tr("rp_game_button", undefined, "Game")}
          icon={<Gamepad2 size={18} />}
          onClick={onGame}
          testId="rp-menu-game"
        />
        <Tile
          label={tr("rp_settings", undefined, "Settings")}
          icon={<Settings2 size={18} />}
          onClick={onSettings}
          testId="rp-open-settings"
        />
        {inFull && (
          <Tile
            label={tr("rp_stop", undefined, "Stop")}
            icon={<Square size={16} />}
            onClick={onStop}
            testId="rp-stop"
            danger
          />
        )}
      </div>
      {!inFull && <CaptureRow onDone={onClose} />}
      {inFull && (
        // Picture size where it shows: in full screen.
        <div className="flex items-center gap-1 rounded-xl bg-white/10 p-1" role="group" data-testid="rp-menu-fit">
          <Scaling size={16} className="mx-1.5 shrink-0 text-white/70" aria-hidden />
          <span className="sr-only">{tr("rp_fit", undefined, "Picture size")}</span>
          {FIT_ORDER.map((f) => (
            <button
              key={f}
              type="button"
              aria-pressed={prefs.fit === f}
              onClick={(e) => {
                e.stopPropagation();
                setPrefs({ fit: f });
              }}
              onDoubleClick={(e) => e.stopPropagation()}
              className={`h-9 min-w-0 flex-1 rounded-lg text-xs font-medium text-white transition-colors ${
                prefs.fit === f ? "bg-white/25" : "hover:bg-white/15"
              }`}
              data-testid={`rp-menu-fit-${f}`}
            >
              {fitName(tr, f)}
            </button>
          ))}
        </div>
      )}
      <div className="flex items-center gap-3 rounded-xl bg-white/10 px-2 py-1.5">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            setPrefs({ muted: !prefs.muted, volume: prefs.volume || 1 });
          }}
          aria-label={
            muted
              ? tr("rp_unmute", undefined, "Sound on")
              : tr("rp_mute", undefined, "Mute")
          }
          aria-pressed={muted}
          className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full text-white hover:bg-white/20"
          data-testid="rp-mute"
        >
          {muted ? <VolumeX size={18} /> : <Volume2 size={18} />}
        </button>
        <VolumeSlider className="h-10 min-w-0 flex-1 accent-white" />
      </div>
    </div>
  );
}
