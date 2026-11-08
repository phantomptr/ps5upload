import { useEffect, useMemo, useRef, useState } from "react";
import { Expand, RotateCw, Volume2 } from "lucide-react";

import { askMotion, motionAvailable, startMotion } from "../lib/motion";
import { PadClient, type PadLinkStage } from "../lib/padClient";
import { TouchControls } from "../screens/Play/TouchControls";
import { PhoneKeyboardButton, TouchModeToggle, TouchpadSurface } from "../screens/Play/TouchpadOverlay";
import { KbmSender } from "../lib/kbmInput";
import { useTr } from "../state/lang";
import { setScreenWakeReason } from "../lib/androidScreenWake";
import { setPadForwardReason } from "../lib/androidPadBridge";
import { rpInput } from "../state/rpInput";
import { makeWatchPlayer, type MsePlayer, type WatchPlayer, type WatchProblem } from "./watchPlayer";
import { OpenOnWifi, useHubDown } from "../join/OpenOnWifi";

type Tr = ReturnType<typeof useTr>;

function stageText(tr: Tr, s: PadLinkStage, computer: string): string {
  switch (s.kind) {
    case "connecting":
      return tr("pad_connecting", undefined, "Connecting…");
    case "asking":
      return tr("pad_asking", { name: computer }, `Waiting for ${computer} to allow this phone…`);
    case "joined":
      return !s.host
        ? tr("pad_no_session", undefined, "Connected. Waiting for Remote Play to start on the computer.")
        : s.passcode && !s.playing
          ? tr("pad_wait_passcode", { name: computer }, `Waiting for the passcode on ${computer}`)
        : s.playing
          ? tr("pad_playing", { host: s.host }, `Controller for ${s.host}`)
          : tr("pad_starting", undefined, "Connected. The stream is starting…");
    case "refused":
      if (s.why === "denied") return tr("pad_denied", undefined, "The computer said no.");
      if (s.why === "no_answer") return tr("pad_no_answer", undefined, "The computer didn't answer in time. Try again.");
      if (s.why === "kicked") return tr("pad_kicked", undefined, "The computer took this phone out of the game.");
      if (s.why === "closed") return tr("pad_link_closed", undefined, "The computer closed the link. Scan the new code to play again.");
      if (s.why === "key") return tr("pad_old_code", undefined, "This code has expired. Scan the new one on the computer.");
      if (s.why === "full") return tr("pad_full", undefined, "Four phones are playing already.");
      if (s.why === "no_stream") return tr("pad_watch_no_stream", undefined, "Nothing is streaming on the computer right now.");
      if (s.why.startsWith("net:")) return netText(tr, s.why.slice(4));
      return tr("pad_refused", undefined, "The computer did not let this phone in.");
    case "reconnecting":
      return tr("pad_reconnecting", undefined, "Connection lost. Reconnecting…");
    case "lost":
      return tr("pad_lost", undefined, "Lost the connection to the computer.");
  }
}

/** Why an internet link could not connect (join/rtcTransport's `net:<why>`), in words. */
export function netText(tr: Tr, why: string): string {
  switch (why) {
    case "mismatch":
      return tr("join_net_mismatch", undefined, "This link doesn't match the host; ask them for a new one.");
    case "no_direct":
      return tr(
        "join_net_no_direct",
        undefined,
        "A direct connection isn't possible from this network (both sides' routers are strict). Try Wi-Fi instead of mobile data, or ask the host to turn on UPnP.",
      );
    case "no_room":
      return tr("pad_old_code", undefined, "This code has expired. Scan the new one on the computer.");
    case "net_full":
      return tr("pad_full", undefined, "Four phones are playing already.");
    case "no_hub":
      return tr("join_net_no_hub", undefined, "Couldn't reach the PS5Upload invite service. Check your internet connection, then try again.");
    // The engine could not take this browser's offer: another browser may do.
    case "offer":
    case "no_webrtc":
      return tr("join_net_no_webrtc", undefined, "This browser can't make direct connections. Try a recent Chrome, Safari or Firefox.");
    default:
      return tr("pad_lost", undefined, "Lost the connection to the computer.");
  }
}

/** A refusal that trying again can't change: only the computer can. */
function canRetry(s: PadLinkStage): boolean {
  return !(s.kind === "refused" && (s.why === "kicked" || s.why === "closed"));
}

/** Fill the screen and hold it sideways, where the browser allows (Android Chrome does). */
async function fillScreen(): Promise<void> {
  try {
    await document.documentElement.requestFullscreen?.({ navigationUI: "hide" });
    await (screen.orientation as ScreenOrientation & { lock?: (o: string) => Promise<void> }).lock?.("landscape");
  } catch {
    // Not here (iPhone Safari): the page still fills the window.
  }
}

type KbmCapsMsg = { mouse: boolean; keyboard: boolean; reason: string | null };

/** Why the touchpad can't be used with this console, in words (null: it can). */
function touchpadBlocked(tr: Tr, caps: KbmCapsMsg | null): string | null {
  if (caps?.mouse) return null;
  if (caps?.reason === "vinput_helper_old" || caps?.reason === "helper_old") return tr("rp_kbm_helper_old", undefined, "Update the PS5 helper to use a keyboard and mouse.");
  if (caps?.reason === "vinput_needs_loader") return tr("rp_kbm_needs_loader", undefined, "Needs a payload loader on port 9021");
  return tr("rp_kbm_unavailable", undefined, "Native keyboard & mouse isn't available on this console yet.");
}

/** The Touchpad & keyboard tab: the screen is the PS5 mouse's touchpad and ⌨ types on its
 *  keyboard, sent through the link to the computer's page (which owns the PS5's devices). */
function PadTouchpad({ client, caps }: { client: PadClient; caps: KbmCapsMsg }) {
  const tr = useTr();
  const sender = useMemo(() => new KbmSender({ send: (m) => client.sendKbm(m) }), [client]);
  useEffect(() => {
    sender.setWant({ mouse: true, keyboard: caps.keyboard });
  }, [sender, caps.keyboard]);
  useEffect(() => () => sender.dispose(), [sender]);
  const input = useMemo(
    () => ({
      move: (dx: number, dy: number) => sender.move(dx, dy),
      button: (b: number, d: boolean) => sender.button(b, d),
      scroll: (dy: number, dx: number) => sender.scroll(dy, dx, 0),
      key: (code: string, d: boolean) => sender.key(code, d),
      shiftHeld: () => sender.shiftHeld(),
    }),
    [sender],
  );
  return (
    <TouchpadSurface sink={input} speed={1.5} className="absolute inset-0 z-10" testId="pad-touchpad">
      <p className="pointer-events-none absolute inset-x-6 top-1/2 -translate-y-1/2 text-center text-xs leading-5 text-white/50">
        {tr(
          "rp_tp_tips",
          undefined,
          "Drag to move. Tap to click. Two fingers: tap to right-click, drag to scroll. Tap, then touch and drag to select.",
        )}
      </p>
      <div className="absolute" style={{ right: "max(12px, env(safe-area-inset-right))", bottom: "max(12px, env(safe-area-inset-bottom))" }}>
        <PhoneKeyboardButton sink={caps.keyboard ? input : null} />
      </div>
    </TouchpadSurface>
  );
}

/**
 * A phone as a controller for the Remote Play session another device runs: the on-screen
 * controller full screen and sideways, as when the phone plays itself, and any controller
 * attached to the phone (Backbone, Kishi, GameSir...) plays too.
 */
export function PadApp({
  url,
  computer: computerName,
  onLeave,
  watch = false,
  transport,
  hello,
  onReconnectToken,
  lanUrl = null,
}: {
  /** An internet link's page on the computer's Wi-Fi ("Open on this Wi-Fi", join/lanHint),
   *  offered when the invite service can't be reached; null: none. */
  lanUrl?: string | null;
  url: string;
  /** The computer's name for the page's words; null (an internet link): "the computer". */
  computer: string | null;
  onLeave?: () => void;
  /** Watch party: watch the stream; play only once the computer hands over the controller. */
  watch?: boolean;
  /** An internet link: the WebRTC transport (join/rtcTransport, a "pad" channel) instead of the
   *  LAN WebSocket, and the hello it opens with (padClient). */
  transport?: typeof WebSocket;
  hello?: Record<string, string | number>;
  /** An internet link: the engine's reconnect token, to keep for this link. */
  onReconnectToken?: (rt: string) => void;
}) {
  const tr = useTr();
  const hubDown = useHubDown(transport ? url : null, lanUrl);
  const computer = computerName ?? tr("pad_net_computer", undefined, "the computer");
  const [stage, setStage] = useState<PadLinkStage>({ kind: "connecting" });
  const [attempt, setAttempt] = useState(0);
  const [portrait, setPortrait] = useState(() => typeof window !== "undefined" && window.innerHeight > window.innerWidth);
  const [light, setLight] = useState<[number, number, number] | null>(null);
  const [client, setClient] = useState<PadClient | null>(null);
  const [motionOn, setMotionOn] = useState(false);
  // Watch party: the picture, whether this page holds the controller, and what went wrong.
  const player = useRef<WatchPlayer | MsePlayer | null>(null);
  const screen = useRef<HTMLDivElement>(null);
  const [control, setControl] = useState(!watch);
  const [problem, setProblem] = useState<WatchProblem | null>(null);
  const [sound, setSound] = useState(false);
  // Touchpad & keyboard: offered when the console has a native mouse (the engine says).
  const [kbmCaps, setKbmCaps] = useState<KbmCapsMsg | null>(null);
  const [tab, setTab] = useState<"controller" | "touchpad">("controller");

  // A phone being a controller (or a screen being watched) has nobody touching it: keep it on,
  // and controllers plugged into it are this page's.
  useEffect(() => {
    setScreenWakeReason("pad-remote", true);
    setPadForwardReason("pad-remote", true);
    return () => {
      setScreenWakeReason("pad-remote", false);
      setPadForwardReason("pad-remote", false);
    };
  }, []);

  useEffect(() => {
    if (!motionOn) return;
    const stop = startMotion((m) => {
      rpInput.motion = m;
    });
    return () => {
      stop();
      rpInput.motion = null;
    };
  }, [motionOn]);

  useEffect(() => {
    const p = watch ? makeWatchPlayer(setProblem, "watch", () => c.askKeyframe()) : null;
    player.current = p;
    if (p) screen.current?.appendChild(p.element);
    setControl(!watch);
    setSound(false);
    const c = new PadClient({
      url,
      WebSocketImpl: transport,
      hello,
      onReconnectToken,
      onStage: setStage,
      touchPad: () => rpInput.touchPad,
      onLight: setLight,
      onKbmCaps: setKbmCaps,
      motion: () => rpInput.motion,
      watch: p
        ? { onFrame: (d) => p.onMessage(d), onCodec: (h) => p.setHevc(h), onControl: setControl }
        : undefined,
    });
    setClient(c);
    c.start();
    // Hidden or out of focus: let go of everything (no stuck stick while the phone is away). In
    // front again: a dropped link comes back now.
    const vis = () => {
      const hidden = document.visibilityState === "hidden";
      c.setNeutral(hidden);
      if (!hidden) c.wake();
    };
    const blur = () => c.setNeutral(true);
    document.addEventListener("visibilitychange", vis);
    window.addEventListener("blur", blur);
    window.addEventListener("focus", vis);
    return () => {
      document.removeEventListener("visibilitychange", vis);
      window.removeEventListener("blur", blur);
      window.removeEventListener("focus", vis);
      c.stop();
      p?.stop();
      p?.element.remove();
    };
    // transport and hello are made once per page (main.tsx).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url, attempt, watch]);

  useEffect(() => {
    const update = () => setPortrait(window.innerHeight > window.innerWidth);
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);

  const ended = stage.kind === "refused" || stage.kind === "lost";
  // Through a short drop the controller stays where the thumbs are.
  const live = stage.kind === "joined" || stage.kind === "reconnecting";
  // The invite service can't be reached (this page's check, or the connection said so): on the
  // computer's Wi-Fi, its own page still works.
  const showLan =
    !!lanUrl && !live && (hubDown || (stage.kind === "refused" && stage.why === "net:no_hub"));
  return (
    <div
      className="fixed inset-0 select-none overflow-hidden bg-black text-white [-webkit-touch-callout:none]"
      style={{ touchAction: "none" }}
      data-testid="pad-app"
    >
      <div className="absolute inset-x-0 top-0 z-20 flex items-center justify-center gap-2 px-3 pt-[max(6px,env(safe-area-inset-top))] text-xs text-white/80">
        <span
          className="inline-block h-2 w-2 shrink-0 rounded-full"
          style={{
            background: stage.kind === "joined"
              ? light
                ? `rgb(${light[0]},${light[1]},${light[2]})`
                : "var(--color-success,#22c55e)"
              : ended
                ? "var(--color-bad,#ef4444)"
                : "#eab308",
          }}
        />
        <span className="truncate" data-testid="pad-stage">
          {watch && stage.kind === "joined" && stage.host && !(stage.passcode && !stage.playing)
            ? control
              ? tr("pad_watch_control", undefined, "Watching, and you have the controller")
              : tr("pad_watch_watching", { host: stage.host }, `Watching ${stage.host}`)
            : stageText(tr, stage, computer)}
        </span>
        {live && typeof document !== "undefined" && !!document.documentElement.requestFullscreen && !document.fullscreenElement && (
          <button
            type="button"
            onClick={() => void fillScreen()}
            className="flex items-center gap-1 rounded-full bg-white/15 px-2 py-1"
            data-testid="pad-full"
          >
            <Expand size={12} />
            {tr("pad_fill", undefined, "Full screen")}
          </button>
        )}
        {live && motionAvailable() && (
          <button
            type="button"
            onClick={() => {
              if (motionOn) setMotionOn(false);
              else void askMotion().then(setMotionOn);
            }}
            aria-pressed={motionOn}
            className={`rounded-full px-2 py-1 ${motionOn ? "bg-white text-black" : "bg-white/15"}`}
            data-testid="pad-motion"
          >
            {tr("pad_motion", undefined, "Motion")}
          </button>
        )}
        {onLeave && (
          <button type="button" onClick={onLeave} className="rounded-full bg-white/15 px-2 py-1" data-testid="pad-leave">
            {tr("pad_leave", undefined, "Stop being a controller")}
          </button>
        )}
      </div>

      {/* Watch party: the stream itself, under everything. */}
      <div ref={screen} className="absolute inset-0 z-0 flex items-center justify-center" aria-hidden />
      {watch && live && !sound && (
        <button
          type="button"
          onClick={() => {
            player.current?.startSound();
            setSound(true);
          }}
          className="absolute bottom-4 left-1/2 z-20 flex -translate-x-1/2 items-center gap-1.5 rounded-full bg-white/90 px-3 py-1.5 text-xs font-medium text-black"
          data-testid="watch-sound"
        >
          <Volume2 size={14} />
          {tr("pad_watch_sound", undefined, "Sound")}
        </button>
      )}
      {watch && problem && (
        <p className="absolute inset-x-4 top-1/2 z-10 -translate-y-1/2 text-center text-sm text-white/85" data-testid="watch-problem">
          {problem === "no_hevc"
            ? tr(
                "pad_watch_no_hevc",
                undefined,
                "This device can't play this stream's picture: the computer streams HEVC, and this device plays only H.264.",
              )
            : tr("pad_watch_decode", undefined, "The picture could not be played here.")}
        </p>
      )}

      {live && control && (tab === "touchpad" && kbmCaps?.mouse && client ? (
        <PadTouchpad client={client} caps={kbmCaps} />
      ) : (
        <TouchControls stream={client} layout="overlay" />
      ))}
      {live && control && (
        <TouchModeToggle
          mode={tab === "touchpad" && kbmCaps?.mouse ? "touchpad" : "controller"}
          onChange={setTab}
          blocked={touchpadBlocked(tr, kbmCaps)}
          touchpadLabel={tr("pad_touchpad_tab", undefined, "Touchpad & keyboard")}
          style={{ top: "calc(max(6px, env(safe-area-inset-top)) + 2rem)" }}
        />
      )}

      {live && portrait && (
        <div className="pointer-events-none absolute inset-x-0 bottom-6 z-20 flex justify-center">
          <span className="flex items-center gap-2 rounded-full bg-black/70 px-3 py-1.5 text-xs text-white/85">
            <RotateCw size={14} />
            {tr("pad_turn", undefined, "Turn the phone sideways")}
          </span>
        </div>
      )}

      {showLan && !ended && lanUrl && (
        <div className="absolute inset-x-0 bottom-8 z-10 flex justify-center px-6">
          <OpenOnWifi href={lanUrl} />
        </div>
      )}
      {ended && (
        <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 px-6 text-center text-sm">
          <p className="max-w-sm text-white/85">{stageText(tr, stage, computer)}</p>
          {showLan && lanUrl && <OpenOnWifi href={lanUrl} />}
          {canRetry(stage) && (
            <button
              type="button"
              onClick={() => setAttempt((n) => n + 1)}
              className="rounded-full bg-white px-4 py-2 text-sm font-medium text-black"
              data-testid="pad-retry"
            >
              {tr("pad_retry", undefined, "Try again")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
