import { useEffect, useRef, useState } from "react";
import { Menu } from "lucide-react";

import { IDLE_PAD, PadButton, touchAt, type PadState, type RemotePlayStream } from "../../lib/remotePlayStream";
import { gamepadsOf } from "../../lib/nativePad";
import { useTr } from "../../state/lang";
import { rpInput, type RpPrefs } from "../../state/remotePlay";
import { controllerScale, thumbOffset, touchOpacity } from "./touchLook";

/** True when the on-screen controller should show: set on, or (auto) a touch screen with no
 *  controller connected. A controller connecting hides it, as PS Remote Play does. */
export function useTouchFirst(mode: RpPrefs["touchControls"]): boolean {
  const coarse = () => typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches;
  const padConnected = () =>
    typeof navigator !== "undefined" && gamepadsOf(navigator).some((g) => !!g?.connected);
  const [state, setState] = useState(() => ({ coarse: coarse(), pad: padConnected() }));
  useEffect(() => {
    const update = () => setState({ coarse: coarse(), pad: padConnected() });
    window.addEventListener("gamepadconnected", update);
    window.addEventListener("gamepaddisconnected", update);
    return () => {
      window.removeEventListener("gamepadconnected", update);
      window.removeEventListener("gamepaddisconnected", update);
    };
  }, []);
  if (mode === "on") return true;
  if (mode === "off") return false;
  return state.coarse && !state.pad;
}

type Part =
  | { kind: "button"; bit: number }
  | { kind: "trigger"; which: "l2" | "r2" }
  | { kind: "stick"; which: "l" | "r"; cx: number; cy: number; radius: number }
  /** `pressed`: the finger is a press of the touchpad (its button), until it slides away. */
  | { kind: "touchpad"; rect: DOMRect; x0: number; y0: number; pressed: boolean };

const STICK_RANGE = 32767;
/** How far (CSS px) a finger on the touchpad moves before it is a swipe, not a press. */
const TOUCHPAD_SLIDE = 12;

/**
 * Where the controller is drawn:
 * - "overlay": over a picture that fills the screen (full screen, a phone sideways). Clear of
 *   the notch and the rounded corners (safe areas); the middle of the picture stays free.
 * - "overlay-bar": the same over a picture that has the button bar along its top (a computer
 *   with the controller switched on), so the top row sits below that bar.
 * - "deck": full screen on a phone held upright, like PS Remote Play's portrait layout: the
 *   picture across the top, the controller on the black below it (`deckTop` is where the
 *   picture ends), so no button covers the game.
 */
export type TouchLayout = "overlay" | "overlay-bar" | "deck";

/** A controller drawn over the picture: two sticks, D-pad, face buttons, shoulders, Options,
 *  Create, PS and a touchpad strip. Several fingers at once; each press buzzes the phone.
 *  Drawn as thin light outlines with a faint fill, so the game shows through; a press fills. */
export function TouchControls({
  stream,
  layout = "overlay",
  opacity = 1,
  size = "m",
  deckTop = "0px",
  onMenu,
}: {
  /** What the presses go to: the stream, or a phone's link to another device's stream. */
  stream: Pick<RemotePlayStream, "sendPad"> | null;
  layout?: TouchLayout;
  opacity?: number;
  size?: RpPrefs["touchSize"];
  deckTop?: string;
  /** Given: a small menu button bottom middle (full screen, where the page has no bar). */
  onMenu?: () => void;
}) {
  const tr = useTr();
  const fingers = useRef(new Map<number, Part>());
  const pad = useRef<PadState>({ ...IDLE_PAD });
  // What is drawn: the pad as last sent (the refs are for the event handlers).
  const [view, setView] = useState<PadState>({ ...IDLE_PAD });
  // Which sticks and the touchpad have a finger on them (drawn brighter).
  const [held, setHeld] = useState({ l: false, r: false, touchpad: false });
  // The space the controller has, to fit it (a tablet gets bigger keys than a phone).
  const root = useRef<HTMLDivElement>(null);
  const [space, setSpace] = useState({ width: 0, height: 0 });
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const measure = () => {
      const r = el.getBoundingClientRect();
      setSpace((s) => (s.width === r.width && s.height === r.height ? s : { width: r.width, height: r.height }));
    };
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(
    () => () => {
      rpInput.touchPad = { ...IDLE_PAD };
    },
    [],
  );

  const apply = () => {
    const p: PadState = { ...IDLE_PAD, touch: null };
    const on = { l: false, r: false, touchpad: false };
    for (const part of fingers.current.values()) {
      if (part.kind === "stick") on[part.which] = true;
      if (part.kind === "touchpad") on.touchpad = true;
      if (part.kind === "button") p.buttons |= part.bit;
      if (part.kind === "trigger") p[part.which] = 255;
      // Pushing the touchpad down is a button of its own on the DualSense (L1+R1+touchpad and
      // other combos read it); a finger that slides is a swipe only.
      if (part.kind === "touchpad" && part.pressed) p.buttons |= PadButton.TOUCHPAD;
    }
    p.lx = pad.current.lx;
    p.ly = pad.current.ly;
    p.rx = pad.current.rx;
    p.ry = pad.current.ry;
    p.touch = pad.current.touch ?? null;
    rpInput.touchPad = p;
    stream?.sendPad();
    setView(p);
    setHeld((h) => (h.l === on.l && h.r === on.r && h.touchpad === on.touchpad ? h : on));
  };

  const moveStick = (part: Extract<Part, { kind: "stick" }>, x: number, y: number) => {
    const dx = Math.max(-1, Math.min(1, (x - part.cx) / part.radius));
    const dy = Math.max(-1, Math.min(1, (y - part.cy) / part.radius));
    pad.current[part.which === "l" ? "lx" : "rx"] = Math.round(dx * STICK_RANGE);
    pad.current[part.which === "l" ? "ly" : "ry"] = Math.round(dy * STICK_RANGE);
  };

  const down = (e: React.PointerEvent, make: (el: HTMLElement) => Part) => {
    e.preventDefault();
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    try {
      // Keeps a finger that slides off the button on it. Refused (InvalidStateError) while the
      // page holds a pointer lock or for a pointer that is no longer active: the press still counts.
      el.setPointerCapture(e.pointerId);
    } catch {
      // Without capture the button still sees its own pointerup and pointercancel.
    }
    const part = make(el);
    fingers.current.set(e.pointerId, part);
    if (part.kind === "stick") moveStick(part, e.clientX, e.clientY);
    if (part.kind === "touchpad") pad.current.touch = touchAt((e.clientX - part.rect.left) / part.rect.width, (e.clientY - part.rect.top) / part.rect.height);
    navigator.vibrate?.(8);
    apply();
  };
  const move = (e: React.PointerEvent) => {
    const part = fingers.current.get(e.pointerId);
    if (!part) return;
    e.stopPropagation();
    if (part.kind === "stick") moveStick(part, e.clientX, e.clientY);
    else if (part.kind === "touchpad") {
      pad.current.touch = touchAt((e.clientX - part.rect.left) / part.rect.width, (e.clientY - part.rect.top) / part.rect.height);
      if (part.pressed && Math.hypot(e.clientX - part.x0, e.clientY - part.y0) > TOUCHPAD_SLIDE) part.pressed = false;
    } else return;
    apply();
  };
  const up = (e: React.PointerEvent) => {
    const part = fingers.current.get(e.pointerId);
    if (!part) return;
    e.stopPropagation();
    fingers.current.delete(e.pointerId);
    if (part.kind === "stick") {
      pad.current[part.which === "l" ? "lx" : "rx"] = 0;
      pad.current[part.which === "l" ? "ly" : "ry"] = 0;
    }
    if (part.kind === "touchpad") pad.current.touch = null;
    apply();
  };
  // Taps on the controller are the controller's: never a click or a double click on the picture
  // (a double tap there must not leave full screen).
  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  const handlers = { onPointerMove: move, onPointerUp: up, onPointerCancel: up, onClick: stop, onDoubleClick: stop };

  const pressed = (bit: number) => (view.buttons & bit) !== 0;

  // The look: a thin light line, a faint fill that brightens while pressed, white glyphs with a
  // soft shadow so they read on a bright scene too.
  const line = (on: boolean) =>
    `border-[1.5px] shadow-[0_0_3px_rgba(0,0,0,0.35)] transition-colors duration-75 ${
      on ? "border-white bg-white/50" : "border-white/70 bg-white/10"
    }`;
  const glyph = "text-white [text-shadow:0_1px_2px_rgba(0,0,0,0.6)]";
  const svgShadow: React.CSSProperties = { filter: "drop-shadow(0 0 1.5px rgba(0,0,0,0.5))" };
  const stroke = (on: boolean) => ({
    stroke: on ? "#fff" : "rgba(255,255,255,0.75)",
    fill: on ? "rgba(255,255,255,0.5)" : "rgba(255,255,255,0.1)",
  });
  // Every key's hit area is its own element at least 44 px across on the screen, however small
  // the controller is drawn (--k); the drawing inside may be smaller.
  // (Inline, as the page's own 44 px minimum for buttons on touch screens counts before the
  // scale and would beat a class.)
  const hit = "pointer-events-auto flex touch-none select-none items-center justify-center bg-transparent p-0";
  const hitSize: React.CSSProperties = { minHeight: "calc(44px / var(--k))", minWidth: "calc(44px / var(--k))" };

  const btn = (label: string, bit: number, cls: string, art: React.ReactNode) => (
    <button
      type="button"
      aria-label={label}
      onPointerDown={(e) => down(e, () => ({ kind: "button", bit }))}
      {...handlers}
      className={`${hit} ${cls}`}
      style={hitSize}
    >
      {art}
    </button>
  );
  /** A shoulder: a rounded tab, its outer corner rounder. */
  const tab = (label: string, on: boolean, side: "l" | "r", top: boolean) => (
    <span
      className={`flex h-[32px] w-[76px] items-center justify-center text-[13px] font-semibold tracking-wide ${glyph} ${line(on)}`}
      style={{
        borderRadius: top ? (side === "l" ? "16px 9px 9px 9px" : "9px 16px 9px 9px") : "9px",
      }}
    >
      {label}
    </span>
  );
  const shoulder = (label: string, bit: number, side: "l" | "r", top: boolean) =>
    btn(label, bit, "h-[44px] w-[84px]", tab(label, pressed(bit), side, top));
  const trig = (label: string, which: "l2" | "r2", side: "l" | "r") => (
    <button
      type="button"
      aria-label={label}
      onPointerDown={(e) => down(e, () => ({ kind: "trigger", which }))}
      {...handlers}
      className={`${hit} h-[44px] w-[84px]`}
      style={hitSize}
    >
      {tab(label, view[which] > 0, side, true)}
    </button>
  );
  /** A D-pad key: a petal with its point toward the empty middle, drawn for Up and turned. */
  const arrow = (label: string, bit: number, turn: number) =>
    btn(
      label,
      bit,
      "h-[52px] w-[52px]",
      <svg viewBox="0 0 52 52" width="52" height="52" aria-hidden style={svgShadow}>
        <g transform={`rotate(${turn} 26 26)`} strokeWidth="1.5" strokeLinejoin="round" {...stroke(pressed(bit))}>
          <path d="M10 9 Q10 3 16 3 L36 3 Q42 3 42 9 L42 30 Q42 34.5 38.5 38 L29.5 47 Q26 50.5 22.5 47 L13.5 38 Q10 34.5 10 30 Z" />
          <path d="M20.5 23 L26 17 L31.5 23" fill="none" stroke="#fff" strokeLinecap="round" strokeWidth="1.8" />
        </g>
      </svg>,
    );
  /** A face button: a thin circle with the PlayStation shape drawn as a line. */
  const face = (label: string, bit: number, shape: React.ReactNode) =>
    btn(
      label,
      bit,
      "h-[52px] w-[52px]",
      <svg viewBox="0 0 52 52" width="52" height="52" aria-hidden style={svgShadow}>
        <circle cx="26" cy="26" r="22.5" strokeWidth="1.5" {...stroke(pressed(bit))} />
        <g fill="none" stroke="#fff" strokeWidth="1.8" strokeLinejoin="round" strokeLinecap="round">
          {shape}
        </g>
      </svg>,
    );
  const pill = (label: string, bit: number) =>
    btn(
      label,
      bit,
      "h-[44px] min-w-[64px] px-[4px]",
      <span
        className={`flex h-[24px] items-center rounded-full px-[10px] text-[10px] font-semibold uppercase tracking-wider ${glyph} ${line(pressed(bit))}`}
      >
        {label}
      </span>,
    );
  const round = (label: string, bit: number) =>
    btn(
      label,
      bit,
      "h-[44px] w-[44px]",
      <span
        className={`flex h-[36px] w-[36px] items-center justify-center rounded-full text-[11px] font-bold ${glyph} ${line(pressed(bit))}`}
      >
        {label}
      </span>,
    );
  const stick = (which: "l" | "r") => {
    const { dx, dy } = thumbOffset(which === "l" ? view.lx : view.rx, which === "l" ? view.ly : view.ry, 34);
    const on = held[which];
    return (
      <div
        aria-label={
          which === "l"
            ? tr("rp_touch_left_stick", undefined, "Left stick")
            : tr("rp_touch_right_stick", undefined, "Right stick")
        }
        onPointerDown={(e) =>
          down(e, (el) => {
            const r = el.getBoundingClientRect();
            return { kind: "stick", which, cx: r.left + r.width / 2, cy: r.top + r.height / 2, radius: r.width / 2 };
          })
        }
        {...handlers}
        className={`pointer-events-auto relative h-[124px] w-[124px] touch-none rounded-full border-[1.5px] shadow-[0_0_3px_rgba(0,0,0,0.3)] transition-colors duration-75 ${
          on ? "border-white/80 bg-white/[0.12]" : "border-white/45 bg-white/[0.05]"
        }`}
      >
        <div
          className={`absolute left-1/2 top-1/2 h-[54px] w-[54px] rounded-full ${line(on)}`}
          style={{ transform: `translate(calc(-50% + ${dx}px), calc(-50% + ${dy}px))` }}
        />
      </div>
    );
  };

  const deck = layout === "deck";
  const k = controllerScale(size, space.width, space.height, deck);
  // A length that grows with the controller: `n` px at scale 1.
  const u = (n: number) => `${n}px * var(--k)`;
  // Clear of the notch, the rounded corners and the home bar.
  const L = "max(12px, env(safe-area-inset-left))";
  const R = "max(12px, env(safe-area-inset-right))";
  const B = "max(12px, env(safe-area-inset-bottom))";
  // The top row: under the bar (overlay-bar); at the top edge clear of the notch otherwise.
  const T = deck ? "8px" : layout === "overlay-bar" ? "3.5rem" : "max(10px, env(safe-area-inset-top))";
  // `base` plus `n` px at scale 1, and never less than `least` px: the hit areas keep 44 px on
  // a controller drawn small, so the gaps between rows do too.
  const plus = (base: string, n: number, least = 0) =>
    !n ? base : least ? `calc(${base} + max(${u(n)}, ${least}px))` : `calc(${base} + ${u(n)})`;
  /** A group drawn `k` times its size, grown from `origin` (the corner or edge it sits by). */
  const at = (origin: string, pos: React.CSSProperties): React.CSSProperties => ({
    position: "absolute",
    ...pos,
    transform: "scale(var(--k))",
    transformOrigin: origin,
  });
  // Upright (deck) the clusters sit in rows on the black; sideways the D-pad and the face
  // buttons sit by the edges at mid height, the sticks lower and further in, toward the thumbs.
  const midY = deck ? undefined : "58%";
  const clusterPos = (side: "left" | "right"): React.CSSProperties =>
    deck
      ? { top: plus(T, 112, 90), [side]: side === "left" ? L : R }
      : { top: midY, translate: "0 -50%", [side]: side === "left" ? L : R };
  const stickPos = (side: "left" | "right"): React.CSSProperties => ({
    bottom: plus(B, deck ? 56 : 0, 44),
    [side]: plus(side === "left" ? L : R, deck ? 40 : 168),
  });
  const menu = !!onMenu && !deck;

  return (
    <div
      ref={root}
      className={`pointer-events-none absolute z-10 select-none [-webkit-touch-callout:none] ${deck ? "inset-x-0 bottom-0" : "inset-0"}`}
      style={{ top: deck ? deckTop : undefined, ["--k" as string]: k }}
      data-testid="rp-touch"
      data-layout={layout}
      data-scale={k}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div className="absolute inset-0" style={{ opacity: touchOpacity(opacity) }}>
        {/* Shoulders, stacked in the top corners (L2 / R2 outermost); sideways, turned toward the
            thumbs. */}
        <div style={at("top left", { top: T, left: L })}>
          <div className="ms-[8px] flex flex-col" style={{ rotate: deck ? undefined : "6deg", transformOrigin: "top left" }}>
            {trig("L2", "l2", "l")}
            {shoulder("L1", PadButton.L1, "l", false)}
          </div>
        </div>
        <div style={at("top right", { top: T, right: R })}>
          <div className="me-[8px] flex flex-col items-end" style={{ rotate: deck ? undefined : "-6deg", transformOrigin: "top right" }}>
            {trig("R2", "r2", "r")}
            {shoulder("R1", PadButton.R1, "r", false)}
          </div>
        </div>
        {/* The touchpad: a faint wide panel, top middle (between the shoulders when upright). */}
        <div
          aria-label={tr("rp_touch_touchpad", undefined, "Touchpad")}
          onPointerDown={(e) =>
            down(e, (el) => ({ kind: "touchpad", rect: el.getBoundingClientRect(), x0: e.clientX, y0: e.clientY, pressed: true }))
          }
          {...handlers}
          className={`pointer-events-auto absolute touch-none rounded-2xl border-[1.5px] shadow-[0_0_3px_rgba(0,0,0,0.3)] transition-colors duration-75 ${
            held.touchpad ? "border-white/80 bg-white/25" : "border-white/40 bg-white/[0.05]"
          }`}
          style={
            deck
              ? { top: plus(T, 6), left: plus(L, 104), right: plus(R, 104), height: `max(44px, calc(${u(64)}))` }
              : {
                  top: T,
                  left: "50%",
                  translate: "-50% 0",
                  width: `min(calc(${u(250)}), 34vw)`,
                  height: `max(44px, calc(${u(56)}))`,
                }
          }
        />
        {/* D-pad: four keys round an empty middle. */}
        <div className="grid grid-cols-3 grid-rows-3" style={at(deck ? "top left" : "left center", clusterPos("left"))}>
          <span />
          {arrow("↑", PadButton.UP, 0)}
          <span />
          {arrow("←", PadButton.LEFT, -90)}
          <span />
          {arrow("→", PadButton.RIGHT, 90)}
          <span />
          {arrow("↓", PadButton.DOWN, 180)}
          <span />
        </div>
        {/* Face buttons: four circles in a diamond. */}
        <div className="grid grid-cols-3 grid-rows-3" style={at(deck ? "top right" : "right center", clusterPos("right"))}>
          <span />
          {face("△", PadButton.TRIANGLE, <path d="M26 16.5 L35 32 L17 32 Z" />)}
          <span />
          {face("□", PadButton.SQUARE, <rect x="18" y="18" width="16" height="16" rx="1" />)}
          <span />
          {face("○", PadButton.CIRCLE, <circle cx="26" cy="26" r="8.5" />)}
          <span />
          {face("✕", PadButton.CROSS, <path d="M19 19 L33 33 M33 19 L19 33" />)}
          <span />
        </div>
        {/* Sticks: lower and toward the middle, where the thumbs rest. */}
        <div style={at("bottom left", stickPos("left"))}>{stick("l")}</div>
        <div style={at("bottom right", stickPos("right"))}>{stick("r")}</div>
        {/* Create and Options: small pills in the bottom corners. */}
        <div style={at("bottom left", { bottom: B, left: L })}>{pill("Create", PadButton.SHARE)}</div>
        <div style={at("bottom right", { bottom: B, right: R })}>{pill("Options", PadButton.OPTIONS)}</div>
        {/* PS: bottom middle, above the menu button when there is one. */}
        <div style={at("bottom center", { bottom: plus(B, menu ? 48 : 0, 48), left: "50%", translate: "-50% 0" })}>
          {round("PS", PadButton.PS)}
        </div>
      </div>
      {/* The menu: never fainter than this, so it can always be found. */}
      {menu && (
        <div
          style={{
            ...at("bottom center", { bottom: B, left: "50%", translate: "-50% 0" }),
            opacity: Math.max(0.7, touchOpacity(opacity)),
          }}
        >
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onMenu?.();
            }}
            onDoubleClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => e.stopPropagation()}
            aria-label={tr("rp_menu", undefined, "Menu")}
            aria-haspopup="dialog"
            className={`${hit} h-[44px] w-[56px]`}
            style={hitSize}
            data-testid="rp-menu-handle"
          >
            <span className={`flex h-[26px] w-[44px] items-center justify-center rounded-full ${glyph} ${line(false)}`}>
              <Menu size={15} strokeWidth={2} aria-hidden />
            </span>
          </button>
        </div>
      )}
    </div>
  );
}
