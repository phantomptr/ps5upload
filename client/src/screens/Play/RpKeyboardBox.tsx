import { useEffect, useRef, useState } from "react";
import { Keyboard } from "lucide-react";

import { useTr } from "../../state/lang";
import { rpInput, useRemotePlayStore } from "../../state/remotePlay";
import { KeyboardTyper } from "../../lib/rpKeyboard";

/** Presses and keys in the box are the box's: never the picture's (it would take the focus
 *  back), never the pad's (the box sits inside the stage, and React events bubble to it). */
const keep = {
  onClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onDoubleClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerUp: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerMove: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onContextMenu: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onKeyDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onKeyUp: (e: { stopPropagation(): void }) => e.stopPropagation(),
};

/** Longest text the box takes (the engine cuts what it sends at 2 KiB). */
const MAX_CHARS = 1000;

/** The game gets an idle pad while the box has the keys, and its own input back after. */
function pauseInput(on: boolean): void {
  rpInput.paused = on;
  if (on) {
    rpInput.held.clear();
    rpInput.touch = null;
  }
  useRemotePlayStore.getState().stream?.sendPad();
}

/**
 * A text box over the stream while the PS5's on-screen keyboard is open (lib/rpKeyboard.ts).
 * It starts with the console's text and takes the focus (a phone opens its keyboard). Typing
 * reaches the console a moment after each pause; Enter or Done keeps the text, Esc or Cancel
 * drops it, and it closes when the console closes its keyboard. It is inside the stage, so it
 * shows in full screen too, and Esc here never leaves full screen or presses Circle.
 */
export function RpKeyboardBox() {
  const kb = useRemotePlayStore((s) => s.keyboard);
  if (!kb) return null;
  // A new opening starts a new box: its own text and its own typer.
  return <KeyboardBox key={kb.seq} initial={kb.text} />;
}

function KeyboardBox({ initial }: { initial: string }) {
  const tr = useTr();
  const [value, setValue] = useState(initial);
  const input = useRef<HTMLInputElement>(null);
  const focused = useRef(false);
  const [typer] = useState(
    () =>
      new KeyboardTyper(
        (t) =>
          useRemotePlayStore
            .getState()
            .stream?.sendKeyboard({ keyboard_text: t }),
        initial,
      ),
  );

  const finish = (accept: boolean) => {
    const st = useRemotePlayStore.getState();
    if (accept) typer.flush();
    else typer.cancel();
    st.stream?.sendKeyboard(
      accept ? { keyboard_accept: true } : { keyboard_reject: true },
    );
    st.closeKeyboard();
  };
  const finishRef = useRef(finish);
  useEffect(() => {
    finishRef.current = finish;
  });

  useEffect(() => {
    // The focus, so typing goes here at once; a phone shows its keyboard.
    input.current?.focus({ preventScroll: true });
    // The caret after the console's text, to add to it.
    const el = input.current;
    el?.setSelectionRange(el.value.length, el.value.length);
    // Esc while the box is open cancels it, focused or not: before the picture's own key
    // handling (Circle, leave full screen), on the document, capturing.
    const onEsc = (e: KeyboardEvent) => {
      // Esc while an input method is composing is the input method's.
      if (e.key !== "Escape" || e.isComposing) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.type === "keydown" && !e.repeat) finishRef.current(false);
    };
    document.addEventListener("keydown", onEsc, true);
    document.addEventListener("keyup", onEsc, true);
    return () => {
      document.removeEventListener("keydown", onEsc, true);
      document.removeEventListener("keyup", onEsc, true);
      typer.cancel();
      if (focused.current) pauseInput(false);
    };
  }, [typer]);

  return (
    <div
      role="dialog"
      aria-label={tr("rp_keyboard_title", undefined, "Type for the PS5")}
      className="pointer-events-auto absolute left-1/2 z-40 flex w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 flex-col gap-2 rounded-2xl border border-white/10 bg-neutral-900/90 p-3 text-white shadow-2xl backdrop-blur-md"
      style={{ top: "max(56px, env(safe-area-inset-top))" }}
      data-testid="rp-keyboard"
      {...keep}
    >
      <div className="flex items-center gap-2 text-xs text-white/70">
        <Keyboard size={14} aria-hidden />
        {tr("rp_keyboard_title", undefined, "Type for the PS5")}
      </div>
      <form
        className="flex items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          finish(true);
        }}
      >
        <input
          ref={input}
          type="text"
          value={value}
          maxLength={MAX_CHARS}
          enterKeyHint="done"
          autoComplete="off"
          autoCorrect="off"
          spellCheck={false}
          aria-label={tr("rp_keyboard_title", undefined, "Type for the PS5")}
          className="min-w-0 flex-1 rounded-lg border border-white/15 bg-black/40 px-3 py-2 text-base text-white outline-none focus:border-[var(--color-accent)]"
          data-testid="rp-keyboard-input"
          onFocus={() => {
            focused.current = true;
            pauseInput(true);
          }}
          onBlur={() => {
            focused.current = false;
            pauseInput(false);
          }}
          onChange={(e) => {
            setValue(e.target.value);
            typer.type(e.target.value);
          }}
        />
        <button
          type="submit"
          className="rounded-full bg-white px-3 py-1.5 text-xs font-medium text-black"
          data-testid="rp-keyboard-done"
        >
          {tr("rp_keyboard_done", undefined, "Done")}
        </button>
        <button
          type="button"
          className="rounded-full bg-white/15 px-3 py-1.5 text-xs"
          onClick={() => finish(false)}
          data-testid="rp-keyboard-cancel"
        >
          {tr("rp_keyboard_cancel", undefined, "Cancel")}
        </button>
      </form>
      <p className="text-[11px] text-white/50">
        {tr(
          "rp_keyboard_hint",
          undefined,
          "Enter: done. Esc: cancel. While you type here, the game gets no keys or controller presses.",
        )}
      </p>
    </div>
  );
}
