import { useEffect, useRef, useState } from "react";
import { LockKeyhole } from "lucide-react";

import { useTr } from "../../state/lang";
import { rpInput, useRemotePlayStore } from "../../state/remotePlay";
import { isPasscode } from "../../lib/remotePlayStream";

/** Presses and keys in the box are the box's: never the picture's, never the pad's (the box sits
 *  inside the stage, and React events bubble to it). */
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

/**
 * The PS5 user's sign-in passcode, when the console asks for it before Remote Play starts (#608).
 * Four digits: a number pad on a phone, the keys on a computer; Enter or OK sends it, Esc or
 * Cancel ends the start. A wrong one asks again. The digits live in this box only while it is
 * open: sent straight to the engine, never stored.
 */
export function RpPasscodeBox() {
  const ask = useRemotePlayStore((s) => s.passcode);
  if (!ask) return null;
  // Each ask is a fresh, empty box.
  return <PasscodeBox key={ask.seq} retry={ask.retry} />;
}

function PasscodeBox({ retry }: { retry: boolean }) {
  const tr = useTr();
  const [value, setValue] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const ok = isPasscode(value);

  const send = () => {
    if (ok) useRemotePlayStore.getState().answerPasscode(value);
  };
  const cancel = () => useRemotePlayStore.getState().cancelPasscode();
  const cancelRef = useRef(cancel);
  useEffect(() => {
    cancelRef.current = cancel;
  });

  useEffect(() => {
    input.current?.focus({ preventScroll: true });
    // The game gets an idle pad while the box is open.
    rpInput.paused = true;
    rpInput.held.clear();
    // Esc cancels, focused or not, before the picture's own Esc handling.
    const onEsc = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.type === "keydown" && !e.repeat) cancelRef.current();
    };
    document.addEventListener("keydown", onEsc, true);
    document.addEventListener("keyup", onEsc, true);
    return () => {
      document.removeEventListener("keydown", onEsc, true);
      document.removeEventListener("keyup", onEsc, true);
      rpInput.paused = false;
    };
  }, []);

  const title = tr("rp_passcode_title", undefined, "Enter this PS5 user's passcode");
  return (
    <div className="pointer-events-auto absolute inset-0 z-40 flex items-center justify-center bg-black/60 p-4" {...keep}>
      <div
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="flex w-[min(22rem,100%)] flex-col gap-3 rounded-2xl border border-white/10 bg-neutral-900/95 p-4 text-white shadow-2xl backdrop-blur-md"
        data-testid="rp-passcode"
      >
        <div className="flex items-center gap-2 text-sm font-medium">
          <LockKeyhole size={16} aria-hidden />
          {title}
        </div>
        <p className="text-xs text-white/70">
          {tr("rp_passcode_body", undefined, "The PS5 asks for it before Remote Play starts. It is sent to the console and not saved.")}
        </p>
        {retry && (
          <p className="text-xs font-medium text-[var(--color-danger,#f87171)]" role="alert" data-testid="rp-passcode-wrong">
            {tr("rp_passcode_wrong", undefined, "Wrong passcode, try again.")}
          </p>
        )}
        <form
          className="flex items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            send();
          }}
        >
          <input
            ref={input}
            type="password"
            inputMode="numeric"
            pattern="[0-9]*"
            maxLength={4}
            autoComplete="off"
            enterKeyHint="go"
            value={value}
            aria-label={title}
            className="min-w-0 flex-1 rounded-lg border border-white/15 bg-black/40 px-3 py-2 text-center text-xl tracking-[0.5em] text-white outline-none focus:border-[var(--color-accent)]"
            data-testid="rp-passcode-input"
            onChange={(e) => setValue(e.target.value.replace(/\D/g, "").slice(0, 4))}
          />
          <button
            type="submit"
            disabled={!ok}
            className="rounded-full bg-white px-4 py-2 text-sm font-medium text-black focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] disabled:opacity-40"
            data-testid="rp-passcode-ok"
          >
            {tr("rp_passcode_ok", undefined, "OK")}
          </button>
          <button
            type="button"
            className="rounded-full bg-white/15 px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
            onClick={cancel}
            data-testid="rp-passcode-cancel"
          >
            {tr("rp_keyboard_cancel", undefined, "Cancel")}
          </button>
        </form>
      </div>
    </div>
  );
}
