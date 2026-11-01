import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router";
import {
  BarChart3,
  Check,
  ChevronDown,
  Expand,
  Gamepad,
  Gamepad2,
  Keyboard,
  Loader2,
  Lock,
  Minimize,
  MonitorPlay,
  MoreHorizontal,
  Mouse,
  PictureInPicture2,
  Play,
  Scaling,
  Settings2,
  Smartphone,
  Square,
  Volume2,
  VolumeX,
} from "lucide-react";

import { Button, Callout, Select, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { deviceName, meshFor, othersRemotePlaying, remotePlayPads, useMeshStore } from "../../state/mesh";
import { isTauriEnv } from "../../lib/tauriEnv";
import { isLinuxDesktop, isMobile } from "../../lib/platform";
import { setScreenWakeReason } from "../../lib/androidScreenWake";
import {
  canStream,
  stopEngineSession,
  touchAt,
  FIT_ORDER,
  type PictureFit,
  type StreamError,
  type StreamPhase,
  type StreamStats,
} from "../../lib/remotePlayStream";
import {
  fetchRunningSession,
  rpInput,
  useRemotePlayStore,
  type RpPrefs,
} from "../../state/remotePlay";
import { TouchControls, useTouchFirst, type TouchLayout } from "./TouchControls";
import {
  canDocFullscreen,
  COARSE_QUERY,
  COMPACT_QUERY,
  docFullscreen,
  focusForControllers,
  holdPageStill,
  inAndroidApp,
  isPhoneSized,
  lockLandscape,
  PHONE_LANDSCAPE_QUERY,
  setWindowFullscreen,
  unlockOrientation,
  useMedia,
} from "./immersive";
import { MicBarButton } from "./MicControl";
import { fitName, PlayMenu, VolumeSlider } from "./PlayMenu";
import { rpBrowserAdvice } from "./browserAdvice";
import { motionAvailable, startMotion } from "../../lib/motion";
import { PadAsks, PhonePadsPanel, usePadLink } from "./PhonePads";
import { GamePanel } from "./game/GamePanel";
import { LaunchStatus } from "./game/LaunchStatus";
import { DropChoice, DropZone, useStreamDropZone } from "./game/DropToInstall";
import { useStreamLaunchRunner } from "./game/streamLaunch";
import { rpControls, useGameControlsStore } from "../../state/gameControls";
import { mouseMoved } from "../../lib/gameControls";
import { captureDelta, engage, isCaptured, releaseCapture, requestCapture, subscribeCapture } from "../../lib/pointerCapture";
import { kbmBlockText, kbmModeLabel, useNativeKbm } from "./useNativeKbm";
import { kbmBlockReason, kbmButtonView, type KbmMode } from "../../lib/kbmInput";
import { shortcutLabel } from "../../lib/kbmShortcuts";
import { KbmCaptureCard, KbmReleaseChip, KbmToast } from "./KbmHints";
import { TouchModeToggle, TouchpadOverlay } from "./TouchpadOverlay";
import { useKbmStore } from "../../state/kbm";
import { useRunningTitleIds } from "../../state/runningApps";
import { useGameLaunch } from "../../lib/useGameLaunch";
import { hostOf } from "../../lib/addr";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { closePip, openPip, pipPadsInBackground, pipSupport } from "./pip";
import { CaptureBarButtons, CaptureOverlay } from "./CaptureControls";
import { LatencyLine, QualityNotice } from "./NetworkExtras";
import { RpBlockerNotice, RpBlockerPhase } from "./RpBlockers";
import { RpKeyboardBox } from "./RpKeyboardBox";
import { pressClosedSettings, SETTINGS_OPENER, SettingsDrawer } from "./SettingsDrawer";
import { beforeRemotePlay } from "../../state/rpBlockers";
import { useRpQualityStore } from "../../state/rpQuality";

type Tr = ReturnType<typeof useTr>;

/** Full screen on an upright phone: where the picture starts, under the menu handle's row. */
const PICTURE_TOP = "calc(max(6px, env(safe-area-inset-top)) + 2.75rem)";

/** Set once the browser advice was dismissed. */
const BROWSER_ADVICE_KEY = "ps5upload.rp_browser_advice_dismissed";

/** How long Esc is held in full screen to leave it (a shorter press is the game's button). */
const ESC_HOLD_MS = 700;
/** How long a quick Esc press holds its button down on the console. */
const ESC_TAP_MS = 80;

/** The plain-English line for an error from the engine or the decoder. */
export function streamErrorMessage(tr: Tr, e: StreamError): string {
  switch (e.code) {
    case "busy":
      return tr(
        "rp_err_busy",
        { s: e.retryS ?? 30 },
        `The PS5 is still finishing the last Remote Play session. Try again in ${e.retryS ?? 30} s.`,
      );
    case "asleep":
    case "wake_failed":
      return tr(
        "rp_err_wake",
        undefined,
        "The PS5 is in rest mode and did not wake. Check that it is plugged in and that Settings › System › Power Saving › Features Available in Rest Mode has Stay Connected to the Internet and Enable Turning On PS5 from Network on.",
      );
    case "unreachable":
      return tr(
        "rp_err_unreachable",
        undefined,
        "The PS5 does not answer on the network. Check that it is on, on the same network, and that Remote Play is enabled in its settings.",
      );
    case "keys":
      return tr(
        "rp_err_keys",
        undefined,
        "The PS5 did not accept this app's pairing. Set up Wake & sign in again for this console.",
      );
    case "refused":
      return tr(
        "rp_err_refused",
        undefined,
        "The PS5 refused to start Remote Play. On the console, check Settings › System › Remote Play › Enable Remote Play (turn it off and on again), press Allow in Play › Set up, or pair again. The code below says which refusal it was.",
      );
    case "rp_off":
      return tr(
        "rp_err_rp_off",
        undefined,
        "Remote Play is switched off on the PS5. On the console, open Settings › System › Remote Play and turn on Enable Remote Play (if it already shows on, turn it off and on again), then press Start.",
      );
    case "passcode":
      return tr(
        "rp_err_passcode",
        undefined,
        "This PS5 user has a sign-in passcode. Remote Play from here cannot enter it; remove the passcode or sign in on the console first.",
      );
    case "video_refused":
      return tr("rp_err_video", undefined, "The PS5 refused that picture size. Try 720p.");
    case "decode":
      return tr("rp_err_decode", undefined, "This device could not decode the picture.");
    case "no_decoder":
      return tr("rp_err_no_decoder", undefined, "This device has no H.264 video decoder, so Remote Play cannot show the picture.");
    case "busy_other":
      return tr(
        "rp_err_busy_other",
        { host: e.host ?? "" },
        `Remote Play is already streaming ${e.host ?? "another console"}. Stop that stream first.`,
      );
    case "lost":
      return tr(
        "rp_err_lost",
        undefined,
        "The connection to the PS5 dropped during the stream. Start it again; if it keeps happening, connect the PS5 or this device by cable.",
      );
    case "socket":
      return tr(
        "rp_err_socket_lost",
        undefined,
        "Lost the connection to the engine and could not get it back within 30 s.",
      );
    default:
      return tr("rp_err_other", undefined, "The stream stopped.");
  }
}

/** What to do about an error, when there is something to do; else null. */
export function streamErrorHelp(tr: Tr, e: StreamError, linux = isLinuxDesktop()): string | null {
  if (e.code !== "no_decoder") return null;
  if (linux) {
    return tr(
      "rp_err_no_decoder_linux",
      undefined,
      "On Linux the picture is decoded by your system's GStreamer. Install its H.264 decoder, then restart the app. Arch, CachyOS, Manjaro: sudo pacman -S gst-libav. Debian, Ubuntu, Mint: sudo apt install gstreamer1.0-libav. Fedora: sudo dnf install gstreamer1-plugin-openh264. Or play from Chrome or Chromium through the web UI.",
    );
  }
  return tr(
    "rp_err_no_decoder_body",
    undefined,
    "Use a current Chrome, Edge or Safari, or the desktop app on Windows or macOS.",
  );
}

export function phaseText(tr: Tr, p: StreamPhase): string {
  switch (p) {
    case "connecting":
      return tr("rp_phase_connecting", undefined, "Connecting…");
    case "waking":
      return tr("rp_phase_waking", undefined, "Waking the PS5 (about 20 s)…");
    case "starting":
      return tr("rp_phase_starting", undefined, "Starting the stream…");
    case "reconnecting":
      return tr("rp_phase_reconnecting", undefined, "Reconnecting…");
    default:
      return "";
  }
}

function StatsLine({
  stats,
  light,
  style,
}: {
  stats: StreamStats;
  light: [number, number, number] | null;
  /** Where it sits when the bottom-left corner is the on-screen controller's. */
  style?: React.CSSProperties;
}) {
  const tr = useTr();
  const v = {
    fps: stats.fps,
    mbps: stats.mbps.toFixed(1),
    ms: stats.decodeMs.toFixed(1),
    codec: stats.codec.startsWith("hev") || stats.codec.startsWith("hvc") ? (stats.hdr ? "HEVC HDR" : "HEVC") : "H.264",
    loss: stats.lossPct.toFixed(2),
    audio: Math.round(stats.audioMs),
  };
  return (
    <div
      className={`pointer-events-none absolute z-20 rounded-md bg-black/60 px-2 py-1 font-mono text-[11px] leading-4 text-white/85 ${style ? "" : "bottom-3 left-3"}`}
      style={style}
      data-testid="rp-stats"
    >
      {light && (
        <span
          className="me-1.5 inline-block h-2 w-2 rounded-full align-middle"
          style={{ background: `rgb(${light[0]},${light[1]},${light[2]})` }}
          title={tr("rp_light_bar", undefined, "Light bar")}
        />
      )}
      {tr(
        "rp_stats_line",
        v,
        `${v.fps} fps · ${v.mbps} Mbit/s · ${v.ms} ms · ${v.codec} · loss ${v.loss}%`,
      )}
      {" · "}
      {tr("rp_stats_audio", { ms: v.audio }, `sound ${v.audio} ms`)}
      <LatencyLine stats={stats} />
    </div>
  );
}

/** A round button on the picture's toolbar. */
function BarButton({
  label,
  onClick,
  active,
  children,
  testId,
  expands,
  disabled,
}: {
  label: string;
  onClick: () => void;
  active?: boolean;
  children: React.ReactNode;
  testId?: string;
  /** Opens and closes a panel (Settings): aria-expanded instead of aria-pressed. */
  expands?: boolean;
  /** Shown but not clickable (the label says why). */
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={expands ? undefined : active}
      aria-expanded={expands ? !!active : undefined}
      aria-disabled={disabled || undefined}
      data-testid={testId}
      {...(expands ? SETTINGS_OPENER : {})}
      onClick={(e) => {
        e.stopPropagation();
        if (!disabled) onClick();
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      className={`flex h-9 w-9 items-center justify-center rounded-full text-white transition-colors ${
        disabled ? "cursor-not-allowed bg-black/40 opacity-50" : `hover:bg-white/20 ${active ? "bg-white/25" : "bg-black/40"}`
      }`}
    >
      {children}
    </button>
  );
}

/** The stream bar's keyboard & mouse button: the running game's mode (Controller, Keyboard &
 *  mouse, Mixed), the next one on a click (as Ctrl+Alt+M), a spinner while the console's helper
 *  starts, and off with the reason when this console takes no keyboard & mouse. */
function KbmBarButton({
  mode,
  title,
  onCycle,
  captured,
}: {
  mode: KbmMode;
  title: string | null;
  onCycle: () => void;
  /** The PS5 has the keyboard and mouse now: a lock on the button, and the release key. */
  captured: boolean;
}) {
  const tr = useTr();
  const caps = useKbmStore((s) => s.caps);
  const status = useKbmStore((s) => s.status);
  const releaseKey = useKbmStore((s) => shortcutLabel(s.shortcuts.release));
  const view = kbmButtonView(mode, caps, status);
  const name = kbmModeLabel(tr, mode);
  const base = view.blocked
    ? kbmBlockText(tr, view.blocked)
    : view.icon === "spinner"
      ? tr("rp_kbm_starting", undefined, "Starting keyboard & mouse on the PS5…")
      : title
        ? tr("rp_kbm_button_game", { mode: name }, `${name} — applies to this game`)
        : tr("rp_kbm_button_home", { mode: name }, `${name} — applies to the home screen`);
  const label = captured
    ? `${base}. ${tr("rp_kbm_release_to", { key: releaseKey }, `Press ${releaseKey} to release the keyboard & mouse`)}`
    : base;
  return (
    <BarButton label={label} onClick={onCycle} disabled={!!view.blocked} testId="rp-kbm-button">
      <span className="relative flex items-center" data-kbm-icon={view.icon} data-testid="rp-kbm-button-icon">
        {captured && (
          <Lock
            size={9}
            strokeWidth={3}
            className="absolute -right-2 -top-2 rounded-full bg-black/80 p-px"
            aria-hidden
            data-testid="rp-kbm-lock"
          />
        )}
        {view.icon === "spinner" ? (
          <Loader2 size={16} className="animate-spin" aria-hidden />
        ) : view.icon === "native" ? (
          <Keyboard size={16} aria-hidden />
        ) : view.icon === "mixed" ? (
          <>
            <Gamepad size={12} aria-hidden />
            <Mouse size={10} aria-hidden />
          </>
        ) : (
          <Gamepad size={16} aria-hidden />
        )}
      </span>
    </BarButton>
  );
}

/** The picture-size menu on the full-screen bar: the current size, and a list to pick another. */
function FitMenu({
  fit,
  open,
  onOpen,
  onPick,
}: {
  fit: PictureFit;
  open: boolean;
  onOpen: (open: boolean) => void;
  onPick: (fit: PictureFit) => void;
}) {
  const tr = useTr();
  const box = useRef<HTMLDivElement>(null);
  // A press anywhere else, or Escape, closes it.
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (!box.current?.contains(e.target as Node)) onOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onOpen(false);
    };
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc, true);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", esc, true);
    };
  }, [open, onOpen]);
  const label = tr("rp_fit", undefined, "Picture size");
  const names: Record<PictureFit, string> = {
    fit: tr("rp_fit_fit", undefined, "Fit (whole picture)"),
    fill: tr("rp_fit_fill", undefined, "Fill (crop the edges)"),
    stretch: tr("rp_fit_stretch", undefined, "Stretch"),
  };
  return (
    <div ref={box} className="relative" onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        aria-label={`${label}: ${fitName(tr, fit)}`}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        data-testid="rp-fit-menu"
        onClick={() => onOpen(!open)}
        className={`flex h-9 items-center gap-1.5 rounded-full px-3 text-sm text-white transition-colors hover:bg-white/20 ${open ? "bg-white/25" : "bg-black/40"}`}
      >
        <Scaling size={16} aria-hidden />
        <span>{fitName(tr, fit)}</span>
        <ChevronDown size={14} aria-hidden className={`transition-transform ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <div
          role="menu"
          aria-label={label}
          className="absolute right-0 top-full z-30 mt-2 min-w-52 rounded-xl bg-black/85 p-1 shadow-[var(--shadow-2)] ring-1 ring-white/15 backdrop-blur"
          data-testid="rp-fit-options"
        >
          {FIT_ORDER.map((f) => (
            <button
              key={f}
              type="button"
              role="menuitemradio"
              aria-checked={fit === f}
              onClick={() => onPick(f)}
              className={`flex w-full items-center gap-2 rounded-lg px-3 py-2 text-start text-sm text-white transition-colors hover:bg-white/15 ${fit === f ? "bg-white/10" : ""}`}
              data-testid={`rp-fit-option-${f}`}
            >
              <Check size={14} aria-hidden className={fit === f ? "opacity-100" : "opacity-0"} />
              {names[f]}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Remote Play: stream a console's screen into the app and play it with a controller, the
 * keyboard or touch. The session belongs to the app (state/remotePlay), not to this screen:
 * leaving it keeps the stream running and coming back shows it again. Uses the Wake & sign in
 * keys stored for the console (never shown); a console without them is pointed at that setup.
 */
export function StreamPanel() {
  const tr = useTr();
  const navigate = useNavigate();
  const profiles = useRosterStore((s) => s.profiles);
  const activeId = useRosterStore((s) => s.active_id);
  const stream = useRemotePlayStore((s) => s.stream);
  const target = useRemotePlayStore((s) => s.target);
  const phase = useRemotePlayStore((s) => s.phase);
  const error = useRemotePlayStore((s) => s.error);
  const stats = useRemotePlayStore((s) => s.stats);
  const light = useRemotePlayStore((s) => s.light);
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const startStream = useRemotePlayStore((s) => s.start);
  const stopStream = useRemotePlayStore((s) => s.stop);
  const setOnScreen = useRemotePlayStore((s) => s.setOnScreen);
  const pip = useRemotePlayStore((s) => s.pip);
  const switching = useRpQualityStore((s) => !!s.switching);
  const pipKind = useMemo(() => pipSupport(), []);
  const [pipFailed, setPipFailed] = useState(false);
  const [profileId, setProfileId] = useState<string>(activeId ?? profiles[0]?.id ?? "");
  const [settingsOpen, setSettingsOpen] = useState(false);
  /** The Game panel over the picture (or under it, on a phone's page). */
  const [gameOpen, setGameOpen] = useState(false);
  const [phonesOpen, setPhonesOpen] = useState(false);
  const [full, setFull] = useState(false);
  const [chrome, setChrome] = useState(true);
  const [fitOpen, setFitOpen] = useState(false);
  // The picture-size menu is only on the full-screen bar.
  useEffect(() => {
    if (!full) setFitOpen(false);
  }, [full]);
  // Touch-sized player (phones, tablets, a narrow window): the bar under the picture and, in
  // full screen, one small menu handle instead of a bar of buttons over the picture.
  const compact = useMedia(COMPACT_QUERY);
  const coarse = useMedia(COARSE_QUERY);
  const portrait = useMedia("(orientation: portrait)");
  const phoneLandscape = useMedia(PHONE_LANDSCAPE_QUERY);
  // Settings as a sheet from the bottom on a phone (either way up), at the right edge elsewhere.
  const settingsSheet = useMedia("(max-width: 639px)") || phoneLandscape;
  const [menuOpen, setMenuOpen] = useState(false);
  // Full screen that came from turning the phone sideways (turning it back leaves it).
  const autoFull = useRef(false);
  // A history entry pushed on entering full screen, so the system back gesture leaves full
  // screen instead of the page (and never stops the stream).
  const backEntry = useRef(false);
  // Whether the last press on the picture was a mouse: only a mouse double-click toggles full
  // screen (a double tap of a finger is play).
  const lastPointer = useRef("mouse");
  const [elsewhere, setElsewhere] = useState<string | null>(null);
  const stage = useRef<HTMLDivElement>(null);
  const canvasHost = useRef<HTMLDivElement>(null);
  const hideTimer = useRef(0);
  const touchFirst = useTouchFirst(prefs.touchControls);
  // The console selected in the app is the one this page is about.
  useEffect(() => {
    if (activeId) setProfileId(activeId);
  }, [activeId]);

  const profile = useMemo(() => profiles.find((p) => p.id === profileId) ?? null, [profiles, profileId]);
  const paired = !!(profile?.wake_regist_key && profile?.wake_rp_key);
  // One stream at a time, for whichever console it plays. This page shows the selected console:
  // the stream when it is that console's, otherwise that console's own Start, with the other
  // stream offered (show it, or stop it and play this one).
  const streaming = !!stream && phase !== null && phase !== "ended";
  const running = streaming && !!profile && target?.host === profile.host;
  // Phones playing along as controllers, and phones asking to (pad_link.rs).
  const [padLink, setPadLink] = usePadLink(running, target?.host ?? null);
  const otherStream = streaming && !running ? target : null;
  const otherProfile = otherStream ? profiles.find((p) => p.host === otherStream.host) ?? null : null;
  // The iOS app hands controllers to the page only while it has the focus: take it on show and
  // whenever a stream starts here.
  useEffect(() => {
    focusForControllers();
  }, [running]);
  // The phone's motion sensors as the controller's, while it plays and motion controls are on.
  useEffect(() => {
    if (!running || !prefs.motion || !motionAvailable()) return;
    const stop = startMotion((m) => {
      rpInput.motion = m;
    });
    return () => {
      stop();
      rpInput.motion = null;
    };
  }, [running, prefs.motion]);
  const playing = running && phase === "playing";
  // Play a title on the stream (from a game's page, or the Game panel): stream, helper, launch.
  const launcher = useGameLaunch();
  // The running game, for its controls (turbo, mouse aim, macros): from the shared running-games
  // watch, nothing polled for it here.
  const runningTitles = useRunningTitleIds(running ? (target?.host ?? null) : null);
  const setControlsTitle = useGameControlsStore((s) => s.setTitle);
  useEffect(() => {
    setControlsTitle(running ? ([...runningTitles][0] ?? null) : null);
  }, [running, runningTitles, setControlsTitle]);
  // Native keyboard & mouse (useNativeKbm.ts): the running game's mode routes keys and the mouse.
  const controlsTitle = useGameControlsStore((s) => s.title);
  const kbm = useNativeKbm({
    stream,
    owner: playing,
    host: running ? (target?.host ?? null) : null,
    title: controlsTitle,
    stage,
    full,
    settingsOpen,
    capturePointer: prefs.capturePointer,
    touchpad: running && touchFirst && prefs.touchMode === "touchpad",
    // Given back: nothing stays held on the pad either, and the stream bar shows (in full
    // screen too).
    onReleased: () => {
      rpInput.held.clear();
      stream?.sendPad();
      showChrome();
    },
  });
  const mouseAim = useGameControlsStore((s) => (s.title ? !!s.byTitle[s.title]?.mouse : false)) && !kbm.mouseNative;
  // Mouse aim: while the pointer is captured on the picture, its movement and buttons are the
  // right stick and the triggers.
  useEffect(() => {
    if (!running || !mouseAim) return;
    const locked = () => isCaptured(stage.current);
    const move = (e: MouseEvent) => {
      const [dx, dy] = captureDelta(e);
      if (locked()) mouseMoved(rpControls.mouse, dx, dy);
    };
    const button = (e: MouseEvent, down: boolean) => {
      if (!locked() && down) return;
      if (e.button === 0) rpControls.mouse.left = down;
      if (e.button === 2) rpControls.mouse.right = down;
    };
    const down = (e: MouseEvent) => button(e, true);
    const up = (e: MouseEvent) => button(e, false);
    const lost = () => {
      if (!locked()) rpControls.mouse.left = rpControls.mouse.right = false;
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mousedown", down);
    document.addEventListener("mouseup", up);
    const unsub = subscribeCapture(lost);
    return () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mousedown", down);
      document.removeEventListener("mouseup", up);
      unsub();
      rpControls.mouse.left = rpControls.mouse.right = false;
    };
  }, [running, mouseAim]);

  // The mouse is the user's again whenever play stops being in front: the window loses the
  // focus or is hidden, a text box of the app (the on-screen keyboard's) takes the keys, the
  // stream stops, or this screen is left (a hidden Activity runs this cleanup too). The browser
  // ends its own lock on most of these; the desktop app's grab only ends here.
  useEffect(() => {
    if (!running) return;
    const onBlur = () => releaseCapture();
    const onVis = () => document.visibilityState === "hidden" && releaseCapture();
    const onFocusIn = (e: FocusEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) releaseCapture();
    };
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVis);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVis);
      document.removeEventListener("focusin", onFocusIn);
      releaseCapture();
    };
  }, [running]);

  // A package dropped on the picture is installed on this console (when the stream ends, by
  // default).
  const dropZone = useStreamDropZone(running ? (target?.host ?? null) : null, running);
  // Another device (a phone, another computer) playing this console: the PS5 takes one at a time.
  const meshState = useMeshStore((s) => (profile ? meshFor(s, profile.host).state : null));
  const otherDevices = useMemo(() => othersRemotePlaying(meshState).map(deviceName), [meshState]);
  // Another device plays Remote Play and takes a phone as its controller: this one can be it.
  const padHosts = useMemo(() => remotePlayPads(meshState), [meshState]);
  // The web UI in a browser Remote Play is not made for says so, until dismissed.
  const [browserAdvice, setBrowserAdvice] = useState(() =>
    safeGetItem(BROWSER_ADVICE_KEY) ? null : rpBrowserAdvice(),
  );

  // On show: controllers drive the console. Off: they do not, but the stream keeps running.
  // Only while it shows this stream: looking at another console must not play this one.
  useEffect(() => {
    if (!running) return;
    setOnScreen(true);
    return () => {
      setOnScreen(false);
      rpInput.held.clear();
      stream?.sendPad();
    };
  }, [setOnScreen, stream, running]);

  // The stream draws on its own canvas; it is shown here whenever this screen is, unless it is
  // in a picture-in-picture window (video picture in picture copies it from here).
  useEffect(() => {
    const host = canvasHost.current;
    if (!stream || !host || !running || pip === "popup" || pip === "document") return;
    host.appendChild(stream.canvas);
    return () => {
      if (stream.canvas.parentElement === host) host.removeChild(stream.canvas);
    };
    // Full screen moves the stage (a portal), so the canvas follows it.
  }, [stream, full, running, pip]);

  // A stream this page does not show (a reload, another window): offer it here.
  useEffect(() => {
    if (streaming) {
      setElsewhere(null);
      return;
    }
    let live = true;
    void fetchRunningSession().then((r) => live && setElsewhere(r.host ?? null));
    return () => {
      live = false;
    };
  }, [streaming]);

  // Two windows: the one you look at takes the stream.
  useEffect(() => {
    if (phase !== "moved") return;
    const onFocus = () => stream?.takeOver();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [phase, stream]);

  const leaveFullLayout = useCallback(() => {
    setFull(false);
    setMenuOpen(false);
    autoFull.current = false;
    void setWindowFullscreen(false);
    unlockOrientation();
  }, []);
  const exitFull = useCallback(() => {
    leaveFullLayout();
    if (backEntry.current) {
      backEntry.current = false;
      window.history.back();
    }
  }, [leaveFullLayout]);
  /** Full screen. `auto`: the phone was turned sideways (no user gesture: the layout only,
   *  the browser's full screen follows on the next touch). */
  const enterFull = useCallback(
    (auto = false) => {
      setFull(true);
      setMenuOpen(false);
      autoFull.current = auto;
      if (!auto) {
        void setWindowFullscreen(true).then(() => {
          if (coarse && isPhoneSized()) lockLandscape();
        });
      } else if (inAndroidApp()) {
        // The app needs no touch to hide the system bars; the turn back still leaves.
        void setWindowFullscreen(true);
      }
      if (compact && !backEntry.current) {
        // Same state as the page's own entry: the router sees no navigation.
        window.history.pushState(window.history.state, "");
        backEntry.current = true;
      }
      stage.current?.focus();
    },
    [coarse, compact],
  );
  const toggleFull = useCallback(() => (full ? exitFull() : enterFull()), [full, exitFull, enterFull]);
  // System back (Android gesture, browser Back) in full screen: leave full screen, keep playing.
  useEffect(() => {
    if (!full) return;
    const onPop = () => {
      if (!backEntry.current) return;
      backEntry.current = false;
      leaveFullLayout();
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, [full, leaveFullLayout]);
  // Turning a phone sideways while playing fills the screen; turning it back (when that is how
  // full screen started) returns to the page. Full screen chosen with the button stays.
  useEffect(() => {
    if (!playing || pip) return;
    if (phoneLandscape && !full) enterFull(true);
    else if (!phoneLandscape && full && autoFull.current) exitFull();
    // Only a turn of the phone (or the stream starting) decides this, not every full change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phoneLandscape, playing]);
  // The stream ended while full screen: back to the page, where Start is.
  useEffect(() => {
    // Not while switching to 720p: the stream stops and starts again in the same place.
    if (full && !running && !switching) exitFull();
  }, [full, running, exitFull, switching]);
  // While the picture fills the screen the page holds still: no scroll, bounce or pull to refresh.
  useEffect(() => {
    if (!full) return;
    holdPageStill(true);
    return () => holdPageStill(false);
  }, [full]);
  // Keep the screen on while playing (a controller does not count as touching the phone).
  useEffect(() => {
    if (!playing) return;
    setScreenWakeReason("remote-play", true);
    return () => setScreenWakeReason("remote-play", false);
  }, [playing]);
  const togglePip = useCallback(async () => {
    if (useRemotePlayStore.getState().pip) return closePip();
    if (full) exitFull();
    const name = target?.name ?? "";
    const ok = await openPip({
      title: tr("rp_pip_title", { name }, `${name} · Remote Play`),
      onBack: () => navigate("/play"),
    });
    setPipFailed(!ok);
  }, [full, exitFull, target, tr, navigate]);
  // The browser left full screen on its own. With a mouse (Esc) the picture goes back in its
  // place. On a touch screen it was most likely the system back gesture or an edge swipe in the
  // middle of play: the picture keeps filling the window and the next touch asks for full
  // screen again; a second Back, or Leave full screen in the menu, returns to the page.
  useEffect(() => {
    if (!full || (isTauriEnv() && !isMobile())) return;
    const onChange = () => {
      if (docFullscreen() || coarse) return;
      exitFull();
    };
    document.addEventListener("fullscreenchange", onChange);
    document.addEventListener("webkitfullscreenchange", onChange);
    return () => {
      document.removeEventListener("fullscreenchange", onChange);
      document.removeEventListener("webkitfullscreenchange", onChange);
    };
  }, [full, coarse, exitFull]);
  // A touch on the picture asks for browser full screen again after Android dropped it, but
  // never a touch on the menu, its handle or its backdrop: that tap may be Leave full screen or
  // Stop, and a pending request would land after them and leave the page in full screen.
  const refullOnTouch = useCallback(
    (e: React.PointerEvent) => {
      if (menuOpen) return;
      const t = e.target as Element | null;
      if (t?.closest?.('[data-testid="rp-menu-handle"],[data-testid="rp-menu-backdrop"],[role="dialog"]')) return;
      if (full && coarse && !docFullscreen() && canDocFullscreen()) void setWindowFullscreen(true);
    },
    [full, coarse, menuOpen],
  );
  // Into or out of full screen the stage is a new element: keep the keyboard on it.
  useEffect(() => {
    if (full) stage.current?.focus();
  }, [full]);
  // Leaving the screen leaves full screen.
  useEffect(() => () => void setWindowFullscreen(false), []);

  const showChrome = useCallback(() => {
    setChrome(true);
    window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => setChrome(false), 2500);
  }, []);
  useEffect(() => () => window.clearTimeout(hideTimer.current), []);

  const start = useCallback(
    async (attach = false, host?: string) => {
      const p = host ? profiles.find((x) => x.host === host) : profile;
      if (!p?.wake_regist_key || !p.wake_rp_key) return;
      const go = async () => {
        await startStream(
          { host: p.host, name: p.name, registKey: p.wake_regist_key!, rpKey: p.wake_rp_key! },
          { attach },
        );
        stage.current?.focus();
        if (compact) stage.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
      };
      // A payload that blocks the picture (Hush) is asked about, or paused, first.
      if (attach) await go();
      else await beforeRemotePlay(p.host, go);
    },
    [profile, profiles, startStream, compact],
  );

  // Another console is streaming: stop it, wait for it to end (the store takes one stream at a
  // time), then start the selected one.
  const switchHere = useCallback(async () => {
    stopStream();
    for (let i = 0; i < 50 && useRemotePlayStore.getState().stream; i++) {
      await new Promise((r) => window.setTimeout(r, 100));
    }
    if (!useRemotePlayStore.getState().stream) await start();
  }, [start, stopStream]);

  useStreamLaunchRunner({
    playingHost: playing && profile ? hostOf(profile.host) : null,
    streamingHost: streaming && target ? hostOf(target.host) : null,
    // The console asked for is the selected one (the runner checks it is the connected one).
    startStream: async () => {
      if (streaming && !running) await switchHere();
      else if (!streaming) await start();
    },
    launch: launcher.launch,
    messages: {
      notConnected: (h) => {
        const name = profiles.find((p) => hostOf(p.host) === h)?.name || h;
        return tr(
          "rp_launch_not_connected",
          { name },
          `Connect to ${name} first (the console list at the top), then press Play again.`,
        );
      },
      noHelper: tr(
        "rp_launch_no_helper",
        undefined,
        "The PS5 is on, but PS5Upload's helper is not running on it, so the game can't be started from here. Send the helper (Connection), then press Play again.",
      ),
    },
  });

  // Esc in full screen: a quick press is its button (Circle), held it leaves full screen. Its
  // button is sent on release, so holding to leave never presses Circle in the game.
  const escHold = useRef<{ timer: number; left: boolean } | null>(null);
  const dropEscHold = useCallback(() => {
    if (escHold.current) window.clearTimeout(escHold.current.timer);
    escHold.current = null;
  }, []);
  useEffect(() => {
    if (!full) dropEscHold();
  }, [full, dropEscHold]);
  useEffect(() => dropEscHold, [dropEscHold]);
  // Esc held while the picture has the mouse (not in full screen, where holding it leaves full
  // screen): the mouse is given back. The browser does this for its own lock; the desktop app's
  // grab has no other way out but switching apps. The key itself still goes to the game.
  const escRelease = useRef<number | null>(null);
  const dropEscRelease = useCallback(() => {
    if (escRelease.current !== null) window.clearTimeout(escRelease.current);
    escRelease.current = null;
  }, []);
  useEffect(() => dropEscRelease, [dropEscRelease]);
  const onKey = useCallback(
    (e: React.KeyboardEvent, down: boolean) => {
      if (e.code === "Escape" && !full) {
        if (!down) dropEscRelease();
        else if (!e.repeat && escRelease.current === null && kbm.captured && prefs.escHoldRelease)
          escRelease.current = window.setTimeout(() => {
            escRelease.current = null;
            kbm.release();
          }, ESC_HOLD_MS);
      }
      if (e.code === "Escape" && full && !e.ctrlKey && !e.altKey && !e.metaKey) {
        kbm.noteKey(e, down);
        e.preventDefault();
        if (down) {
          if (e.repeat || escHold.current) return;
          const hold = {
            left: false,
            timer: window.setTimeout(() => {
              hold.left = true;
              kbm.release();
              exitFull();
            }, ESC_HOLD_MS),
          };
          escHold.current = hold;
          return;
        }
        const hold = escHold.current;
        dropEscHold();
        if (!hold || hold.left) return;
        // Native keyboard: the PS5 keyboard's Esc.
        if (kbm.escTap()) return;
        // Not a key for the game (or the keyboard is this computer's): a press leaves full
        // screen, as everywhere else.
        if (!running || kbm.keysToApp || !useRemotePlayStore.getState().keymap.Escape) {
          exitFull();
          return;
        }
        rpInput.held.add("Escape");
        stream?.sendPad();
        window.setTimeout(() => {
          rpInput.held.delete("Escape");
          stream?.sendPad();
        }, ESC_TAP_MS);
        return;
      }
      if (kbm.onKey(e, down)) return;
      // Released in Keyboard & mouse or Mixed: the keys are this computer's.
      if (kbm.keysToApp && down) return;
      if (!running || !useRemotePlayStore.getState().keymap[e.code]) return;
      e.preventDefault();
      if (down) rpInput.held.add(e.code);
      else rpInput.held.delete(e.code);
      stream?.sendPad();
    },
    [full, exitFull, stream, running, dropEscHold, dropEscRelease, kbm, prefs.escHoldRelease],
  );

  // Shift + drag on the picture is a finger on the touchpad (for games that use it, like
  // maps and menus); the position on the picture is the position on the pad.
  const onPointer = useCallback(
    (e: React.PointerEvent, kind: "down" | "move" | "up") => {
      if (kind === "down") lastPointer.current = e.pointerType;
      // The bar stays hidden while the picture has the pointer (a click there is the game's).
      if (e.pointerType === "mouse" && kind === "move" && !isCaptured(stage.current)) showChrome();
      if (!running) return;
      if (kind === "up") {
        if (!rpInput.touch) return;
        rpInput.touch = null;
      } else {
        if (kind === "down" && !e.shiftKey) return;
        if (kind === "move" && !rpInput.touch) return;
        const r = e.currentTarget.getBoundingClientRect();
        rpInput.touch = touchAt((e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height);
      }
      stream?.sendPad();
    },
    [stream, showChrome, running],
  );

  // Clicking the picture takes the keyboard and (if set, or always in a mode that sends the mouse
  // to the PS5) captures the mouse (lib/pointerCapture.ts: the browser's lock, else the desktop
  // app's grab); Esc (held, in the app) gives it back, so Circle is Backspace too. Only a mouse
  // press captures it:
  // a finger has no pointer to hide, and a lock taken from a tap (a touch-screen laptop, Linux
  // Chrome) freezes every later touch's position and makes the touch controller's
  // setPointerCapture throw, so its buttons stop answering.
  const capture = useCallback(() => {
    const el = stage.current;
    if (!el) return;
    el.focus();
    if (lastPointer.current !== "mouse") {
      // A finger: no pointer to capture; a keyboard attached to the tablet plays the PS5.
      if (kbm.mouseNative) engage(el);
      return;
    }
    if (!(prefs.capturePointer || kbm.mouseNative) || !document.hasFocus() || isCaptured(el)) return;
    requestCapture(el);
  }, [prefs.capturePointer, kbm.mouseNative]);

  if (!canStream()) {
    return (
      <Callout tone="warn" title={tr("rp_no_webcodecs", undefined, "This browser cannot play a stream")}>
        {tr(
          "rp_no_webcodecs_body",
          undefined,
          "Remote Play here decodes video with WebCodecs, which this browser does not have. Use a current Chrome, Edge or Safari, or the desktop app on Windows or macOS.",
        )}
      </Callout>
    );
  }

  const elsewhereProfile = elsewhere ? profiles.find((p) => p.host === elsewhere) : null;
  const showBar = !playing || (chrome && !kbm.locked) || settingsOpen || fitOpen || gameOpen;
  const muted = prefs.muted || prefs.volume === 0;
  // Full screen on an upright phone: the picture across the top, the controller below it.
  const portraitFull = compact && full && portrait;
  // The on-screen controller: under the bar on a computer; over the picture in full screen
  // sideways; on the black under the picture in full screen upright. On a phone or tablet the
  // page is view only (a scroll or a tap for the app's own bars must never press a button on
  // the console): Full screen is where the controller is. A controller or keyboard connected
  // to the device plays on the page too.
  const showTouch = running && touchFirst && !pip;
  const touchLayout: TouchLayout = !compact ? "overlay-bar" : portraitFull ? "deck" : "overlay";
  const touchHere = showTouch && (!compact || full);
  const touchOverPicture = touchHere && touchLayout !== "deck";
  // Sideways in full screen the controller has the menu button (bottom middle), not the handle.
  const touchMenu = compact && full && touchHere && touchLayout === "overlay";
  // The touch screen as the PS5 mouse's touchpad, where the console has a native mouse.
  const touchpadOn = prefs.touchMode === "touchpad" && !!kbm.hostCaps?.mouse;
  /** Characters the PS5 keyboard has no key for: into the PS5's text box when it is open. */
  const typeInPs5Box = (text: string): boolean => {
    const el = document.querySelector<HTMLInputElement>('[data-testid="rp-keyboard-input"]');
    if (!el) return false;
    el.focus({ preventScroll: true });
    el.setSelectionRange(el.value.length, el.value.length);
    return document.execCommand("insertText", false, text);
  };
  // One Settings everywhere (the stream bar, the menu, beside Start): the drawer over the
  // stream, in full screen too, so opening it never leaves full screen.
  const openSettings = () => {
    setMenuOpen(false);
    setGameOpen(false);
    setFitOpen(false);
    setSettingsOpen(true);
  };
  const toggleSettings = () => (settingsOpen ? setSettingsOpen(false) : openSettings());
  const settingsDrawer = (
    <SettingsDrawer
      open={settingsOpen}
      onClose={() => setSettingsOpen(false)}
      running={running}
      layout={settingsSheet ? "sheet" : "side"}
      contained={full}
      underBar={full && !compact && running}
      // Over the picture (full screen, a phone) a press beside it is the drawer's, never play.
      backdrop={full || settingsSheet}
      onFocusBack={() => {
        if (running) stage.current?.focus({ preventScroll: true });
      }}
    />
  );
  const stopHere = () => {
    if (full) exitFull();
    setMenuOpen(false);
    stopStream();
  };
  const menuProps = {
    pipKind,
    onClose: () => setMenuOpen(false),
    onExitFull: exitFull,
    onPip: () => {
      setMenuOpen(false);
      void togglePip();
    },
    onSettings: openSettings,
    onStop: stopHere,
    onGame: () => {
      setMenuOpen(false);
      setSettingsOpen(false);
      setGameOpen(true);
    },
  };

  const stageView = (
        <div
          ref={stage}
          tabIndex={0}
          onKeyDown={(e) => onKey(e, true)}
          onKeyUp={(e) => onKey(e, false)}
          onBlur={(e) => {
            rpInput.held.clear();
            stream?.sendPad();
            // Focus gone from the picture: keys and buttons held on the PS5 are let go too.
            if (!(e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget))) kbm.releaseAll();
          }}
          // A press that only closed Settings is not play.
          onClick={running ? (e) => !e.shiftKey && !pressClosedSettings() && capture() : undefined}
          onDoubleClick={
            running
              ? (e) => {
                  // The stage moves into a portal on body: an event left to bubble on reaches
                  // React a second time there and would toggle straight back.
                  e.stopPropagation();
                  // A finger's double tap is play (or a mis-tap), never full screen.
                  if (lastPointer.current === "mouse") toggleFull();
                }
              : undefined
          }
          onPointerUpCapture={refullOnTouch}
          onContextMenu={running ? (e) => e.preventDefault() : undefined}
          onPointerDown={(e) => onPointer(e, "down")}
          onPointerMove={(e) => onPointer(e, "move")}
          onPointerUp={(e) => onPointer(e, "up")}
          onPointerLeave={(e) => onPointer(e, "up")}
          className={`group overflow-hidden bg-black outline-none ${
            full
              ? // Pinned to the window's own edges (fixed inset-0; with `relative` also on it,
                // relative won and the stage sat in the page, cut at the top and bottom), as a
                // video player's full screen is; never taller than a phone's visible height.
                "fixed inset-0 z-[200] max-h-[100dvh] touch-none select-none overscroll-none [-webkit-touch-callout:none]"
              : "relative mx-auto aspect-video w-full max-w-[calc((100dvh_-_12rem)*16/9)] rounded-[var(--radius-card)] shadow-[var(--shadow-2)] focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
          } ${playing && !chrome && !(compact && full) ? "cursor-none" : ""}`}
          data-full={full ? (compact ? "immersive" : "bar") : undefined}
          data-testid="rp-stage"
          {...dropZone.webProps}
        >
          <div
            ref={canvasHost}
            // The picture layer, under everything drawn over it. It never takes a pointer: the
            // stage's handlers get every press, and controls over it are hit first everywhere.
            className="rp-place pointer-events-none absolute inset-0 z-0 flex items-center justify-center"
            style={
              portraitFull
                ? { top: PICTURE_TOP, bottom: "auto", height: "56.25vw" }
                : full
                  ? {
                      paddingLeft: "env(safe-area-inset-left)",
                      paddingRight: "env(safe-area-inset-right)",
                      paddingTop: "env(safe-area-inset-top)",
                      paddingBottom: "env(safe-area-inset-bottom)",
                    }
                  : undefined
            }
          />

          <LaunchStatus />
          <RpBlockerPhase host={profile?.host ?? null} />
          <DropZone active={dropZone.active} />
          <DropChoice consoleName={target?.name ?? ""} />
          {running && phase !== "playing" && phase !== "moved" && (
            <div className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-black/40 text-sm text-white/85">
              <Spinner size={16} tone="inherit" />
              {phaseText(tr, phase)}
            </div>
          )}
          {running && phase === "moved" && (
            <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-black/70 text-sm text-white/85">
              <MonitorPlay size={28} />
              {tr("rp_moved", undefined, "Playing in another window.")}
              <Button size="sm" variant="primary" onClick={() => stream?.takeOver()}>
                {tr("rp_play_here", undefined, "Play here")}
              </Button>
            </div>
          )}
          {playing && kbm.captureHint && !settingsOpen && !pip && (
            <div
              className={`pointer-events-none absolute left-1/2 z-10 -translate-x-1/2 rounded-full bg-black/70 px-3 py-1 text-xs text-white/85 ${showBar && !compact ? "top-16" : "top-3"}`}
              data-testid="rp-capture-hint"
            >
              {tr("rp_capture_hint", undefined, "Click the picture to capture the mouse")}
            </div>
          )}
          {playing && kbm.pointerHeld && prefs.kbmChip && !settingsOpen && !pip && !kbm.hint && (
            <KbmReleaseChip low={showBar && !compact} />
          )}
          {playing && kbm.hint && !settingsOpen && !pip && (
            <KbmCaptureCard key={kbm.hint.n} reason={kbm.hint.reason} onDone={kbm.dismissHint} />
          )}
          {playing && kbm.toast && !pip && <KbmToast key={kbm.toast.n} mode={kbm.toast.mode} captured={kbm.toast.captured} />}
          {running && <PadAsks pending={padLink.pending} />}
          {running && <RpKeyboardBox />}
          {running && pip && (
            <div
              className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-black/80 px-6 text-center text-sm text-white/85"
              data-testid="rp-pip-on"
            >
              <PictureInPicture2 size={28} />
              {tr("rp_pip_on", undefined, "Showing in picture in picture.")}
              <span className="max-w-md text-xs text-white/60">
                {pip === "video"
                  ? tr(
                      "rp_pip_hint_video",
                      undefined,
                      "This browser's picture in picture shows the picture only: the keyboard and controller play it while this window is in front.",
                    )
                  : pipPadsInBackground(pip)
                    ? tr(
                        "rp_pip_hint_any",
                        undefined,
                        "A controller keeps playing it while you use other apps. Click the small window to play with the keyboard.",
                      )
                    : tr(
                        "rp_pip_hint_front",
                        undefined,
                        "The keyboard and controller play it while the small window is the active one: click it. On macOS and Linux a controller only reaches the window in front.",
                      )}
              </span>
              <Button size="sm" variant="primary" onClick={() => closePip()} data-testid="rp-pip-bring-back">
                {tr("rp_pip_bring_back", undefined, "Bring it back here")}
              </Button>
            </div>
          )}
          {!running && (
            <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 text-sm text-white/60">
              <Gamepad2 size={28} />
              {tr("rp_idle", undefined, "Press Start to play this PS5 here.")}
            </div>
          )}
          {playing && prefs.showStats && stats && (
            <StatsLine
              stats={stats}
              light={light}
              // The controller has the bottom-left corner: the stats go on the picture's top
              // left (upright) or top middle under the touchpad row.
              style={
                portraitFull
                  ? { top: `calc(${PICTURE_TOP} + 6px)`, left: "8px" }
                  : touchOverPicture
                    ? {
                        left: "50%",
                        translate: "-50% 0",
                        top: touchLayout === "overlay-bar" ? "7rem" : "calc(max(10px, env(safe-area-inset-top)) + 6rem)",
                      }
                    : undefined
              }
            />
          )}

          {running && !compact && (
            <div
              className={`absolute inset-x-0 top-0 z-20 flex items-center gap-2 bg-gradient-to-b from-black/70 to-transparent px-3 pb-6 pt-[max(0.75rem,env(safe-area-inset-top))] transition-opacity duration-200 ${
                showBar ? "opacity-100" : "pointer-events-none opacity-0"
              }`}
              onClick={(e) => e.stopPropagation()}
              onDoubleClick={(e) => e.stopPropagation()}
              data-testid="rp-toolbar"
            >
              <span className="min-w-0 flex-1 truncate text-sm font-medium text-white">
                <span className="me-2 inline-block h-2 w-2 rounded-full bg-[var(--color-success,#22c55e)] align-middle" />
                {target?.name}
              </span>
              <BarButton
                label={tr("rp_show_stats_short", undefined, "Stats")}
                active={prefs.showStats}
                onClick={() => setPrefs({ showStats: !prefs.showStats })}
                testId="rp-toggle-stats"
              >
                <BarChart3 size={16} />
              </BarButton>
              <CaptureBarButtons />
              <BarButton
                label={muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
                active={muted}
                onClick={() => setPrefs({ muted: !prefs.muted, volume: prefs.volume || 1 })}
              >
                {muted ? <VolumeX size={16} /> : <Volume2 size={16} />}
              </BarButton>
              {/* Volume right on the picture (Settings has it too); keys on it never reach the pad. */}
              <VolumeSlider className="w-20 accent-white sm:w-28" />
              <MicBarButton />
              {full && (
                // Picture size where it shows: in full screen (on the page the box is the
                // picture's own shape, so every size looks the same).
                <FitMenu
                  fit={prefs.fit}
                  open={fitOpen}
                  onOpen={setFitOpen}
                  onPick={(fit) => {
                    setPrefs({ fit });
                    setFitOpen(false);
                  }}
                />
              )}
              {!compact && (
                <BarButton
                  label={tr("rp_phone_title", undefined, "Use a phone as a controller")}
                  active={phonesOpen || padLink.pads.length > 0}
                  onClick={() => {
                    if (full) exitFull();
                    setPhonesOpen((v) => !v);
                  }}
                  testId="rp-phones"
                >
                  <Smartphone size={16} />
                </BarButton>
              )}
              {playing && <KbmBarButton mode={kbm.mode} title={controlsTitle} onCycle={kbm.cycle} captured={kbm.captured} />}
              <BarButton
                label={tr("rp_game_button", undefined, "Game")}
                active={gameOpen}
                onClick={() => {
                  setSettingsOpen(false);
                  setGameOpen((v) => !v);
                }}
                testId="rp-game"
              >
                <Gamepad2 size={16} />
              </BarButton>
              <BarButton
                label={tr("rp_settings", undefined, "Settings")}
                active={settingsOpen}
                expands
                onClick={toggleSettings}
                testId="rp-settings-button"
              >
                <Settings2 size={16} />
              </BarButton>
              {pipKind && (
                <BarButton
                  label={
                    pip
                      ? tr("rp_pip_exit", undefined, "Leave picture in picture")
                      : tr("rp_pip", undefined, "Picture in picture")
                  }
                  active={!!pip}
                  onClick={() => void togglePip()}
                  testId="rp-pip"
                >
                  <PictureInPicture2 size={16} />
                </BarButton>
              )}
              <BarButton
                label={full ? tr("rp_exit_fullscreen", undefined, "Leave full screen") : tr("rp_fullscreen", undefined, "Full screen")}
                active={full}
                onClick={toggleFull}
                testId="rp-fullscreen"
              >
                {full ? <Minimize size={16} /> : <Expand size={16} />}
              </BarButton>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  if (full) exitFull();
                  stopStream();
                }}
                className="flex h-9 items-center gap-1.5 rounded-full bg-[var(--color-danger,#dc2626)] px-3 text-sm font-medium text-white hover:brightness-110"
                data-testid="rp-stop"
              >
                <Square size={13} />
                {tr("rp_stop", undefined, "Stop")}
              </button>
            </div>
          )}
          {running && <CaptureOverlay />}
          <QualityNotice />
          {touchHere && touchpadOn && (
            <TouchpadOverlay
              input={kbm.touch}
              speed={prefs.touchpadSpeed}
              deck={touchLayout === "deck"}
              deckTop={`calc(${PICTURE_TOP} + 56.25vw)`}
              onMenu={touchMenu ? () => setMenuOpen(true) : undefined}
              onUntypeable={typeInPs5Box}
            />
          )}
          {touchHere && !touchpadOn && (
            <TouchControls
              stream={stream}
              layout={touchLayout}
              opacity={prefs.touchOpacity}
              size={prefs.touchSize}
              deckTop={`calc(${PICTURE_TOP} + 56.25vw)`}
              onMenu={touchMenu ? () => setMenuOpen(true) : undefined}
            />
          )}
          {touchHere && playing && (
            <TouchModeToggle
              mode={touchpadOn ? "touchpad" : "controller"}
              onChange={(m) => setPrefs({ touchMode: m })}
              blocked={kbm.hostCaps?.mouse ? null : kbmBlockText(tr, kbmBlockReason(kbm.hostCaps))}
              style={{
                top:
                  touchLayout === "deck"
                    ? `calc(${PICTURE_TOP} + 56.25vw + 8px)`
                    : touchLayout === "overlay-bar"
                      ? "4rem"
                      : "max(8px, env(safe-area-inset-top))",
              }}
            />
          )}

          {/* Full screen on a touch-sized player: one small handle, top middle, opens the menu.
              Nothing else on the picture leaves full screen or stops the stream. */}
          {compact && full && !menuOpen && !touchMenu && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen(true);
              }}
              onDoubleClick={(e) => e.stopPropagation()}
              onPointerDown={(e) => e.stopPropagation()}
              aria-label={tr("rp_menu", undefined, "Menu")}
              aria-haspopup="dialog"
              className="absolute left-1/2 z-30 flex h-8 w-14 -translate-x-1/2 items-center justify-center rounded-full border border-white/20 bg-black/40 text-white/80 opacity-70 backdrop-blur-sm transition-opacity hover:opacity-100 active:opacity-100"
              style={{ top: "max(6px, env(safe-area-inset-top))" }}
              data-testid="rp-menu-handle"
            >
              <MoreHorizontal size={18} />
            </button>
          )}
          {compact && full && menuOpen && (
            <>
              {/* A tap beside the menu closes it, and nothing else. */}
              <div
                className="absolute inset-0 z-30 bg-black/30"
                onClick={(e) => {
                  e.stopPropagation();
                  setMenuOpen(false);
                }}
                onDoubleClick={(e) => e.stopPropagation()}
                onPointerDown={(e) => e.stopPropagation()}
                data-testid="rp-menu-backdrop"
              />
              <div
                role="dialog"
                aria-label={tr("rp_menu", undefined, "Menu")}
                className="absolute left-1/2 z-40 w-[min(22rem,calc(100%-2rem))] -translate-x-1/2 overflow-y-auto rounded-2xl border border-white/10 bg-neutral-900/90 p-2 shadow-2xl backdrop-blur-md"
                style={{
                  top: "max(8px, env(safe-area-inset-top))",
                  maxHeight: "calc(100% - max(8px, env(safe-area-inset-top)) - max(8px, env(safe-area-inset-bottom)))",
                }}
                onClick={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                onPointerDown={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Escape") setMenuOpen(false);
                }}
                onKeyUp={(e) => e.stopPropagation()}
              >
                <PlayMenu inFull {...menuProps} />
              </div>
            </>
          )}
          {launcher.dialogs}
          {full && settingsDrawer}
          {/* Over the picture: in full screen, and on a computer's page. */}
          {(full || !compact) && (
            <GamePanel
              host={target?.host ?? null}
              open={gameOpen && running}
              onClose={() => setGameOpen(false)}
              variant={compact ? "sheet" : "side"}
            />
          )}
        </div>
  );

  return (
    <div className="space-y-3">
      {!running && (
        // Start first (10.0.1); the console is the one selected in the app (no picker of its own).
        <div className="flex flex-wrap items-end gap-2">
          <Button
            variant="primary"
            size="md"
            onClick={() => void (otherStream ? switchHere() : start())}
            disabled={!paired}
            data-testid="rp-start"
          >
            <Play size={14} />
            {otherStream
              ? tr("rp_switch_start", undefined, "Stop the other stream and start")
              : tr("rp_start", undefined, "Start")}
          </Button>
          <Select
            label={tr("rp_quality", undefined, "Picture")}
            block={false}
            className="w-28"
            value={prefs.res}
            onChange={(e) => setPrefs({ res: e.target.value as RpPrefs["res"] })}
            options={[
              { value: "1920x1080", label: "1080p" },
              { value: "1280x720", label: "720p" },
            ]}
          />
          <Select
            label={tr("rp_fps", undefined, "Frame rate")}
            block={false}
            className="w-28"
            value={String(prefs.fps)}
            onChange={(e) => setPrefs({ fps: Number(e.target.value) as RpPrefs["fps"] })}
            options={[
              { value: "60", label: "60 fps" },
              { value: "30", label: "30 fps" },
            ]}
          />
          <Button
            variant="ghost"
            size="md"
            onClick={toggleSettings}
            aria-expanded={settingsOpen}
            data-testid="rp-settings-button"
            {...SETTINGS_OPENER}
          >
            <Settings2 size={14} />
            {tr("rp_settings", undefined, "Settings")}
          </Button>
        </div>
      )}

      {otherStream && (
        <Callout
          tone="info"
          title={tr(
            "rp_streaming_other",
            { name: otherStream.name },
            `Remote Play is streaming ${otherStream.name}`,
          )}
          action={
            <div className="flex gap-2">
              {otherProfile && (
                <Button size="sm" variant="primary" onClick={() => setProfileId(otherProfile.id)}>
                  {tr("rp_show_it", undefined, "Show it")}
                </Button>
              )}
              <Button size="sm" onClick={() => stopStream()}>
                {tr("rp_stop", undefined, "Stop")}
              </Button>
            </div>
          }
        >
          {tr(
            "rp_streaming_other_body",
            undefined,
            "One console streams at a time. It keeps playing while you look at this one; Start here stops it first.",
          )}
        </Callout>
      )}

      {!running && browserAdvice && (
        <div data-testid="rp-browser-advice">
        <Callout
          tone="info"
          title={tr("rp_browser_best", undefined, "Remote Play works best in Chrome or Edge")}
          onDismiss={() => {
            safeSetItem(BROWSER_ADVICE_KEY, "1");
            setBrowserAdvice(null);
          }}
        >
          {browserAdvice === "firefox"
            ? tr(
                "rp_browser_firefox",
                undefined,
                "In Firefox, Esc always leaves full screen, so it can't be the controller's ○ there, and picture in picture isn't available.",
              )
            : browserAdvice === "safari"
              ? tr(
                  "rp_browser_safari",
                  undefined,
                  "In Safari, Esc always leaves full screen, so it can't be the controller's ○ there, and picture in picture shows the picture only.",
                )
              : browserAdvice === "other_mobile"
                ? tr("rp_browser_other_mobile", undefined, "It's made and tested for Chrome on Android.")
                : tr(
                    "rp_browser_other",
                    undefined,
                    "It's made and tested for them. Here, Esc as the controller's ○ in full screen and picture in picture may not work.",
                  )}
        </Callout>
        </div>
      )}
      {!running && otherDevices.length > 0 && (
        <Callout
          tone="warn"
          title={tr(
            "rp_other_device_playing",
            { name: otherDevices.join(", ") },
            `${otherDevices.join(", ")} is using Remote Play on this console`,
          )}
        >
          {tr(
            "rp_other_device_playing_body",
            undefined,
            "A PS5 plays Remote Play with one device at a time: starting here ends it there.",
          )}
          {padHosts.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-2">
              {padHosts.map(({ device, pad }) => (
                <Button
                  key={device.id}
                  size="sm"
                  variant="primary"
                  leftIcon={<Smartphone size={14} />}
                  onClick={() =>
                    navigate(`/play/pad?url=${encodeURIComponent(pad)}&name=${encodeURIComponent(deviceName(device))}`)
                  }
                  data-testid="rp-be-controller"
                >
                  {padHosts.length > 1
                    ? tr("rp_phone_be_for", { name: deviceName(device) }, `Be ${deviceName(device)}'s controller`)
                    : tr("rp_phone_be", undefined, "Use this device as its controller")}
                </Button>
              ))}
            </div>
          )}
        </Callout>
      )}

      {!streaming && elsewhere && (
        <Callout
          tone="info"
          title={tr(
            "rp_running_elsewhere",
            { name: elsewhereProfile?.name ?? elsewhere },
            `Remote Play is running on ${elsewhereProfile?.name ?? elsewhere}`,
          )}
          action={
            <div className="flex gap-2">
              {elsewhereProfile?.wake_regist_key && (
                <Button size="sm" variant="primary" onClick={() => void start(true, elsewhere)}>
                  {tr("rp_show_here", undefined, "Show it here")}
                </Button>
              )}
              <Button
                size="sm"
                onClick={() => {
                  stopEngineSession(elsewhere);
                  setElsewhere(null);
                }}
              >
                {tr("rp_stop", undefined, "Stop")}
              </Button>
            </div>
          }
        >
          {tr(
            "rp_running_elsewhere_body",
            undefined,
            "It was started from another window or before this page reloaded, and is still going.",
          )}
        </Callout>
      )}

      {profile && !paired && !running && (
        <Callout
          tone="info"
          title={tr("play_not_ready", undefined, "Set up Play for this console first")}
          action={
            <Button size="sm" variant="primary" onClick={() => navigate("/play/setup")} data-testid="play-open-setup">
              {tr("play_setup_title", undefined, "Set up Play")}
            </Button>
          }
        >
          {tr(
            "play_not_ready_body",
            undefined,
            "A few steps, once per console: the checklist does what it can for you and ticks each step as it's done.",
          )}
        </Callout>
      )}

      {error && (
        <Callout tone="error" title={streamErrorMessage(tr, error)}>
          {streamErrorHelp(tr, error) ?? error.detail}
        </Callout>
      )}
      <RpBlockerNotice host={profile?.host ?? null} error={error} running={running} />
      {pipFailed && (
        <Callout tone="warn" title={tr("rp_pip_failed", undefined, "Picture in picture did not open.")}>
          {tr(
            "rp_pip_failed_body",
            undefined,
            "The browser or the system refused the window. Try again from the button on the picture.",
          )}
        </Callout>
      )}

      {phonesOpen && running && (
        <PhonePadsPanel
          consoleHost={target?.host ?? null}
          link={padLink}
          onLink={setPadLink}
          onClose={() => setPhonesOpen(false)}
        />
      )}

      {!full && settingsDrawer}

      {full ? createPortal(stageView, document.body) : stageView}

      {/* Touch-sized player, on the page: the bar is under the picture, never on it, so every
          action fits at 360 px and nothing covers the game. More opens the rest. */}
      {compact && running && !full && (
        <div className="space-y-2">
          <div
            className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] bg-neutral-900 px-3 py-2 text-white"
            data-testid="rp-toolbar"
          >
            <span className="w-full min-w-0 truncate text-xs font-medium text-white/80">
              <span className="me-2 inline-block h-2 w-2 rounded-full bg-[var(--color-success,#22c55e)] align-middle" />
              {target?.name}
            </span>
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                enterFull();
              }}
              className="flex h-11 min-w-0 flex-1 items-center justify-center gap-1.5 rounded-full bg-white/15 px-3 text-sm font-medium hover:bg-white/25"
              data-testid="rp-fullscreen"
            >
              <Expand size={15} />
              {tr("rp_fullscreen", undefined, "Full screen")}
            </button>
            <button
              type="button"
              onClick={() => setMenuOpen((v) => !v)}
              aria-label={tr("rp_more", undefined, "More")}
              aria-expanded={menuOpen}
              className={`flex h-11 w-11 shrink-0 items-center justify-center rounded-full hover:bg-white/25 ${menuOpen ? "bg-white/25" : "bg-white/15"}`}
              data-testid="rp-more"
            >
              <MoreHorizontal size={18} />
            </button>
            <button
              type="button"
              onClick={stopHere}
              className="flex h-11 shrink-0 items-center gap-1.5 rounded-full bg-[var(--color-danger,#dc2626)] px-4 text-sm font-medium hover:brightness-110"
              data-testid="rp-stop"
            >
              <Square size={13} />
              {tr("rp_stop", undefined, "Stop")}
            </button>
          </div>
          {menuOpen && (
            <div className="rounded-[var(--radius-card)] bg-neutral-900 p-2">
              <PlayMenu inFull={false} {...menuProps} />
            </div>
          )}
          {/* The Game panel: under the picture and its bar, where there is room. */}
          <GamePanel
            host={target?.host ?? null}
            open={gameOpen}
            onClose={() => setGameOpen(false)}
            variant="inline"
          />
          {showTouch && (
            <p className="flex items-center gap-2 px-1 text-xs text-[var(--color-muted)]" data-testid="rp-touch-in-full">
              <Gamepad2 size={14} className="shrink-0" />
              {tr(
                "rp_view_only",
                undefined,
                "View only here. Tap Full screen to play with the on-screen controller. A controller or keyboard connected to this device plays here too.",
              )}
            </p>
          )}
        </div>
      )}


      {compact && !running && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "rp_touch_hint",
            undefined,
            "Turn the phone sideways, or tap Full screen, to fill the screen. There, the small ⋯ at the top opens the menu; taps on the picture are play and never leave full screen. Back leaves full screen.",
          )}
        </p>
      )}
      {!compact && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "rp_keys_body3",
            undefined,
            "Click the picture to play with the keyboard; Esc gives the pointer back. Double-click for full screen; there a quick Esc is Circle and holding Esc leaves full screen. Keys, sound and more are in Settings.",
          )}
        </p>
      )}
    </div>
  );
}
