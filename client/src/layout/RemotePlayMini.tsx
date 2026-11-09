import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { Volume2, VolumeX, X } from "lucide-react";

import { Spinner } from "../components";
import { useTr } from "../state/lang";
import { useRemotePlayStore } from "../state/remotePlay";
import { installAndroidPip, useAndroidPipStore } from "../lib/androidPip";
import {
  DRAG_SLOP_PX,
  loadCorner,
  miniPlayerVisible,
  nearestCorner,
  saveCorner,
  type MiniCorner,
} from "../lib/miniPlayer";
import { phaseText } from "../lib/rpPhaseText";

/**
 * The Remote Play mini player (#637): on any other screen of the app, the running stream's
 * picture in a small floating window, dragged to any corner (remembered). A tap goes back to
 * Play; × hides it until Play is opened again; the speaker mutes.
 *
 * The stream belongs to the app (lib/remotePlayStream): its one canvas moves into whichever
 * place shows it, as the desktop pop-out does (screens/Play/pip.tsx). Each place takes it only
 * when the others cannot (lib/miniPlayer miniPlayerVisible): the Play screen while on show, a
 * picture-in-picture window, Android's picture in picture, then here. Controllers and keys go
 * to the console only while the Play screen is on show (state/remotePlay readPad), so this
 * window takes no input of the app's.
 *
 * Mounted once, above the per-console screen trees (App.tsx).
 */
export function RemotePlayMini() {
  const tr = useTr();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const stream = useRemotePlayStore((s) => s.stream);
  const phase = useRemotePlayStore((s) => s.phase);
  const target = useRemotePlayStore((s) => s.target);
  const onScreen = useRemotePlayStore((s) => s.onScreen);
  const pip = useRemotePlayStore((s) => s.pip);
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const pictureHere = useRemotePlayStore((s) => s.pictureHere);
  const androidPip = useAndroidPipStore((s) => s.inPip);
  const [dismissed, setDismissed] = useState(false);
  const [corner, setCorner] = useState<MiniCorner>(loadCorner);
  // A drop: animates from where it was let go to its corner (bumped on each drop).
  const [drop, setDrop] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const canvasHost = useRef<HTMLDivElement>(null);
  const drag = useRef<{ id: number; x: number; y: number; moved: boolean } | null>(null);
  const dropFrom = useRef<DOMRect | null>(null);

  // × holds until the Play screen shows the stream again, or a new stream starts.
  useEffect(() => {
    if (onScreen) setDismissed(false);
  }, [onScreen]);
  useEffect(() => setDismissed(false), [stream]);

  const visible = miniPlayerVisible({
    streaming: !!stream,
    phase,
    pathname,
    onScreen,
    pip,
    androidPip,
    enabled: prefs.miniPlayer !== false,
    dismissed,
    pictureHere,
  });

  useEffect(() => {
    const host = canvasHost.current;
    if (!visible || !stream || !host) return;
    return stream.mount(host);
  }, [visible, stream]);

  // From where it was dropped to its corner.
  useLayoutEffect(() => {
    const el = box.current;
    const from = dropFrom.current;
    dropFrom.current = null;
    if (!el || !from) return;
    const to = el.getBoundingClientRect();
    el.style.transition = "none";
    el.style.transform = `translate(${from.left - to.left}px, ${from.top - to.top}px)`;
    void el.getBoundingClientRect();
    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    el.style.transition = still ? "none" : "transform 180ms ease-out";
    el.style.transform = "";
  }, [drop]);

  if (!visible || !stream) return null;

  const muted = prefs.muted || prefs.volume === 0;
  const name = target?.name ?? "";
  const backLabel = tr("rp_mini_back", { name }, `Back to Play: ${name}`);
  const openPlay = () => navigate("/play");

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    drag.current = { id: e.pointerId, x: e.clientX, y: e.clientY, moved: false };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // A synthetic pointer: the moves still arrive here.
    }
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    const dx = e.clientX - d.x;
    const dy = e.clientY - d.y;
    if (!d.moved && Math.hypot(dx, dy) < DRAG_SLOP_PX) return;
    d.moved = true;
    e.currentTarget.style.transition = "none";
    e.currentTarget.style.transform = `translate(${dx}px, ${dy}px)`;
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    drag.current = null;
    if (!d.moved) {
      openPlay();
      return;
    }
    const r = e.currentTarget.getBoundingClientRect();
    const c = nearestCorner(r.left + r.width / 2, r.top + r.height / 2, window.innerWidth, window.innerHeight);
    dropFrom.current = r;
    // Laid out at its corner with no offset, then animated from the drop (layout effect above).
    e.currentTarget.style.transform = "";
    setCorner(c);
    saveCorner(c);
    setDrop((n) => n + 1);
  };
  const onPointerCancel = (e: React.PointerEvent<HTMLDivElement>) => {
    drag.current = null;
    e.currentTarget.style.transform = "";
  };
  // The buttons are buttons: no drag, no tap-through to Play.
  const own = {
    onPointerDown: (e: React.PointerEvent) => e.stopPropagation(),
    onPointerUp: (e: React.PointerEvent) => e.stopPropagation(),
  };

  return (
    <div
      ref={box}
      role="button"
      tabIndex={0}
      aria-label={backLabel}
      title={backLabel}
      data-corner={corner}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          openPlay();
        }
      }}
      className="rp-mini group fixed z-[45] aspect-video cursor-pointer touch-none select-none overflow-hidden rounded-xl bg-black shadow-[0_10px_30px_rgba(0,0,0,0.45)] ring-1 ring-white/15 outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
      data-testid="rp-mini"
    >
      {/* rp-place: the picture sizes itself against this box (index.css). */}
      <div ref={canvasHost} className="rp-place pointer-events-none absolute inset-0 flex items-center justify-center" />
      {phase !== "playing" && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center gap-2 bg-black/40 px-2 text-center text-xs text-white/85">
          <Spinner size={14} tone="inherit" />
          {phase ? phaseText(tr, phase) : null}
        </div>
      )}
      <div className="absolute end-1 top-1 flex gap-1">
        <button
          type="button"
          {...own}
          onClick={(e) => {
            e.stopPropagation();
            setPrefs({ muted: !prefs.muted, volume: prefs.volume || 1 });
          }}
          aria-label={muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
          title={muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
          aria-pressed={muted}
          className="flex h-[28px] w-[28px] items-center justify-center rounded-full bg-black/55 text-white hover:bg-white/25"
          data-testid="rp-mini-mute"
        >
          {muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
        <button
          type="button"
          {...own}
          onClick={(e) => {
            e.stopPropagation();
            setDismissed(true);
          }}
          aria-label={tr("rp_mini_close", undefined, "Hide the mini player (the stream keeps running)")}
          title={tr("rp_mini_close", undefined, "Hide the mini player (the stream keeps running)")}
          className="flex h-[28px] w-[28px] items-center justify-center rounded-full bg-black/55 text-white hover:bg-white/25"
          data-testid="rp-mini-close"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}

/**
 * Android's picture in picture (lib/androidPip.ts): the app's window is the small floating one
 * over other apps, so it shows only the picture, filling it, over everything else. Also keeps
 * the activity told whether a stream plays (mounted once, App.tsx).
 */
export function AndroidPipPicture() {
  const tr = useTr();
  const inPip = useAndroidPipStore((s) => s.inPip);
  const stream = useRemotePlayStore((s) => s.stream);
  const host = useRef<HTMLDivElement>(null);
  useEffect(() => installAndroidPip(), []);
  useEffect(() => {
    const el = host.current;
    if (!inPip || !stream || !el) return;
    return stream.mount(el);
  }, [inPip, stream]);
  if (!inPip) return null;
  // The stream ended in the small window: say so rather than show the app's screens in it, until
  // the activity closes the window (setPipActive, android-postinit-patch.sh step 12).
  if (!stream) {
    return (
      <div
        className="fixed inset-0 z-[2147483000] flex items-center justify-center bg-black p-2 text-center text-xs text-white"
        data-testid="rp-android-pip-ended"
      >
        {tr("rp_android_pip_ended", undefined, "Stream ended")}
      </div>
    );
  }
  return (
    <div
      ref={host}
      className="rp-place fixed inset-0 z-[2147483000] flex items-center justify-center bg-black"
      data-testid="rp-android-pip"
    />
  );
}
