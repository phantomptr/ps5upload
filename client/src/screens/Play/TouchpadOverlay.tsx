// A touch screen as the PS5's mouse and keyboard (native keyboard & mouse with nothing attached):
// the picture is a touchpad (lib/touchpadGestures.ts) and ⌨ opens the phone's keyboard, whose
// characters go to the PS5 as key presses (lib/charToHid.ts). Used over the stream on a phone or
// tablet (TouchpadOverlay) and on the phone link page (pad/PadApp.tsx, Touchpad & keyboard).

import { useEffect, useRef, useState } from "react";
import { Gamepad2, Keyboard, Menu, Mouse } from "lucide-react";

import { useTr } from "../../state/lang";
import { TouchpadGestures, type TouchpadSink } from "../../lib/touchpadGestures";
import { namedKey, pressKey, textDiff, typeText } from "../../lib/charToHid";

/** Where typed keys go: the PS5's keyboard (KbmSender.key or the phone link's sender). */
export interface KeySink {
  key: (code: string, down: boolean) => void;
}

export type TouchpadInput = TouchpadSink & KeySink;

/** The surface: fingers on it move, click and scroll the PS5's mouse. */
export function TouchpadSurface({
  sink,
  speed,
  className = "",
  style,
  children,
  testId = "rp-touchpad",
}: {
  sink: TouchpadSink | null;
  speed: number;
  className?: string;
  style?: React.CSSProperties;
  children?: React.ReactNode;
  testId?: string;
}) {
  const sinkRef = useRef(sink);
  const speedRef = useRef(speed);
  useEffect(() => {
    sinkRef.current = sink;
    speedRef.current = speed;
  });
  // Made on the first touch (it reads the latest sink and speed through the refs).
  const made = useRef<TouchpadGestures | null>(null);
  const gestures = () =>
    (made.current ??= new TouchpadGestures(
      {
        move: (dx, dy) => sinkRef.current?.move(dx, dy),
        button: (b, d) => sinkRef.current?.button(b, d),
        scroll: (dy, dx) => sinkRef.current?.scroll(dy, dx),
      },
      () => speedRef.current,
    ));
  // Whatever is held is let go when the surface goes away.
  useEffect(() => () => made.current?.cancel(), []);
  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  return (
    <div
      className={`touch-none select-none [-webkit-touch-callout:none] ${className}`}
      style={style}
      data-testid={testId}
      onPointerDown={(e) => {
        stop(e);
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // A pointer that cannot be captured still moves while over the surface.
        }
        gestures().down(e.pointerId, e.clientX, e.clientY, e.timeStamp);
      }}
      onPointerMove={(e) => {
        stop(e);
        gestures().move(e.pointerId, e.clientX, e.clientY);
      }}
      onPointerUp={(e) => {
        stop(e);
        gestures().up(e.pointerId, e.timeStamp);
      }}
      onPointerCancel={(e) => {
        stop(e);
        gestures().cancel();
      }}
      onClick={stop}
      onDoubleClick={stop}
      onContextMenu={(e) => {
        e.preventDefault();
        stop(e);
      }}
    >
      {children}
    </div>
  );
}

/** What the hidden input holds when nothing is typed: a Backspace has something to delete. */
const SENTINEL = "  ";
/** Past this, the hidden input starts again from SENTINEL (between words). */
const RESET_AT = 40;

/**
 * ⌨: opens the phone's keyboard (a hidden text field) and sends what is typed as key presses.
 * `onUntypeable` gets characters the PS5 keyboard has no key for (é, 日本); true when it took
 * them (the PS5's text box was open), else a short hint shows.
 */
export function PhoneKeyboardButton({
  sink,
  onUntypeable,
  className = "",
}: {
  sink: KeySink | null;
  onUntypeable?: (text: string) => boolean;
  className?: string;
}) {
  const tr = useTr();
  const input = useRef<HTMLInputElement>(null);
  const last = useRef(SENTINEL);
  const composing = useRef(false);
  const [open, setOpen] = useState(false);
  const [hint, setHint] = useState(false);
  useEffect(() => {
    if (!hint) return;
    const id = window.setTimeout(() => setHint(false), 3000);
    return () => window.clearTimeout(id);
  }, [hint]);

  const resetIfIdle = () => {
    const el = input.current;
    if (!el || composing.current) return;
    if (!el.value.startsWith(SENTINEL) || el.value.length > RESET_AT) {
      el.value = SENTINEL;
      last.current = SENTINEL;
    }
  };
  const apply = () => {
    const el = input.current;
    if (!el || !sink) return;
    const { deleted, inserted } = textDiff(last.current, el.value);
    last.current = el.value;
    for (let i = 0; i < deleted; i++) pressKey(sink, "Backspace");
    const skipped = typeText(sink, inserted);
    if (skipped && !onUntypeable?.(skipped)) setHint(true);
    resetIfIdle();
  };

  return (
    <>
      <button
        type="button"
        aria-label={tr("rp_tp_keyboard", undefined, "Keyboard")}
        title={tr("rp_tp_keyboard", undefined, "Keyboard")}
        aria-pressed={open}
        disabled={!sink}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          const el = input.current;
          if (!el) return;
          if (open) {
            el.blur();
            return;
          }
          el.value = SENTINEL;
          last.current = SENTINEL;
          // In the press itself: a phone opens its keyboard only for a focus a tap asked for.
          el.focus({ preventScroll: true });
          el.setSelectionRange(el.value.length, el.value.length);
        }}
        className={`flex h-11 w-11 items-center justify-center rounded-full border border-white/25 bg-black/45 text-white backdrop-blur-sm disabled:opacity-40 ${open ? "bg-white/30" : ""} ${className}`}
        data-testid="rp-tp-keyboard"
      >
        <Keyboard size={18} aria-hidden />
      </button>
      <input
        ref={input}
        type="text"
        defaultValue={SENTINEL}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        spellCheck={false}
        enterKeyHint="send"
        aria-label={tr("rp_tp_keyboard", undefined, "Keyboard")}
        // Kept on screen (a field off screen may not open a keyboard) but invisible and tiny.
        className="pointer-events-none fixed bottom-0 left-1/2 h-px w-px opacity-0"
        data-testid="rp-tp-keyboard-input"
        data-rp-typing=""
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (!sink || e.nativeEvent.isComposing) return;
          // Keys that type nothing: sent as they are (a phone keyboard that only edits the text
          // reports "Unidentified", and its Backspace shows up in the text instead).
          const code = namedKey(e.key);
          if (!code) return;
          e.preventDefault();
          pressKey(sink, code);
        }}
        onKeyUp={(e) => e.stopPropagation()}
        onCompositionStart={() => {
          composing.current = true;
        }}
        onCompositionEnd={() => {
          composing.current = false;
          apply();
        }}
        onInput={apply}
      />
      {hint && (
        <div
          className="pointer-events-none fixed bottom-20 left-1/2 z-[300] w-max max-w-[min(22rem,calc(100%-2rem))] -translate-x-1/2 rounded-full bg-black/80 px-3 py-1.5 text-center text-xs text-white"
          data-testid="rp-tp-untypeable"
        >
          {tr(
            "rp_tp_untypeable",
            undefined,
            "Only English letters, numbers and symbols can be typed here. Use a text field on the PS5 for others.",
          )}
        </div>
      )}
    </>
  );
}

/** The on-screen controller or the touchpad: a small switch over the picture. Off (with why)
 *  when the console has no native mouse. */
export function TouchModeToggle({
  mode,
  onChange,
  blocked,
  style,
  touchpadLabel,
}: {
  mode: "controller" | "touchpad";
  /** The touchpad's name (the phone link page says "Touchpad & keyboard"). */
  touchpadLabel?: string;
  onChange: (m: "controller" | "touchpad") => void;
  /** Why the touchpad cannot be used here, or null. */
  blocked: string | null;
  style?: React.CSSProperties;
}) {
  const tr = useTr();
  const item = (m: "controller" | "touchpad", icon: React.ReactNode, label: string) => {
    const off = m === "touchpad" && !!blocked;
    return (
      <button
        type="button"
        role="radio"
        aria-checked={mode === m}
        aria-disabled={off || undefined}
        title={off ? blocked! : label}
        onClick={(e) => {
          e.stopPropagation();
          if (!off) onChange(m);
        }}
        onPointerDown={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
        className={`flex h-8 items-center gap-1 rounded-full px-2.5 text-xs ${mode === m ? "bg-white text-black" : "text-white"} ${off ? "opacity-40" : ""}`}
        data-testid={`rp-touch-mode-${m}`}
      >
        {icon}
        <span>{label}</span>
      </button>
    );
  };
  return (
    <div
      role="radiogroup"
      aria-label={tr("rp_tp_mode", undefined, "Touch controls")}
      className="pointer-events-auto absolute left-1/2 z-30 flex -translate-x-1/2 gap-0.5 rounded-full border border-white/20 bg-black/50 p-0.5 backdrop-blur-sm"
      style={style}
      data-testid="rp-touch-mode"
    >
      {item("controller", <Gamepad2 size={14} aria-hidden />, tr("rp_kbm_controller", undefined, "Controller"))}
      {item("touchpad", <Mouse size={14} aria-hidden />, touchpadLabel ?? tr("rp_tp_touchpad", undefined, "Touchpad"))}
    </div>
  );
}

/** The touchpad over the stream: the whole picture (or the deck below it, upright), with ⌨ and
 *  the menu. */
export function TouchpadOverlay({
  input,
  speed,
  deck,
  deckTop,
  onMenu,
  onUntypeable,
}: {
  input: TouchpadInput | null;
  speed: number;
  /** Upright full screen: the area under the picture is the touchpad. */
  deck: boolean;
  deckTop: string;
  onMenu?: () => void;
  onUntypeable?: (text: string) => boolean;
}) {
  const tr = useTr();
  const [tips, setTips] = useState(true);
  useEffect(() => {
    const id = window.setTimeout(() => setTips(false), 5000);
    return () => window.clearTimeout(id);
  }, []);
  return (
    <TouchpadSurface
      sink={input}
      speed={speed}
      className={`absolute z-10 ${deck ? "inset-x-0 bottom-0 border-t border-white/10 bg-white/[0.03]" : "inset-0"}`}
      style={deck ? { top: deckTop } : undefined}
    >
      {tips && (
        <p
          className="pointer-events-none absolute left-1/2 top-1/2 w-max max-w-[calc(100%-2rem)] -translate-x-1/2 -translate-y-1/2 rounded-xl bg-black/60 px-3 py-2 text-center text-xs leading-5 text-white/85"
          data-testid="rp-touchpad-tips"
        >
          {tr(
            "rp_tp_tips",
            undefined,
            "Drag to move. Tap to click. Two fingers: tap to right-click, drag to scroll. Tap, then touch and drag to select.",
          )}
        </p>
      )}
      <div
        className="absolute flex items-center gap-2"
        style={{ right: "max(12px, env(safe-area-inset-right))", bottom: "max(12px, env(safe-area-inset-bottom))" }}
      >
        <PhoneKeyboardButton sink={input} onUntypeable={onUntypeable} />
      </div>
      {onMenu && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onMenu();
          }}
          onPointerDown={(e) => e.stopPropagation()}
          aria-label={tr("rp_menu", undefined, "Menu")}
          aria-haspopup="dialog"
          className="absolute left-1/2 flex h-[26px] w-[44px] -translate-x-1/2 items-center justify-center rounded-full border border-white/40 bg-black/40 text-white"
          style={{ bottom: "max(12px, env(safe-area-inset-bottom))" }}
          data-testid="rp-menu-handle"
        >
          <Menu size={15} strokeWidth={2} aria-hidden />
        </button>
      )}
    </TouchpadSurface>
  );
}
