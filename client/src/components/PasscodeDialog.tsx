import { useEffect, useRef, useState } from "react";
import { LockKeyhole } from "lucide-react";

import { isPasscode } from "../lib/passcode";
import { useTr } from "../state/lang";

/** Presses and keys in the box are the box's: never the picture's, never the pad's (over the
 *  stream the box sits inside the stage, and React events bubble to it). */
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
 * The PS5 user's sign-in passcode box (#608), shared by Remote Play (over the stream) and Wake &
 * sign in (over the app). Four digits: a number pad on a phone, the keys on a computer; Enter or
 * OK sends, Esc or Cancel cancels. The digits live in this box only while it is open: handed to
 * `onSubmit`, never stored. Mount it with a fresh `key` per ask so each ask starts empty.
 */
export function PasscodeDialog({
  title,
  body,
  retry,
  sending = false,
  onSubmit,
  onCancel,
  placement = "screen",
  testId = "passcode",
}: {
  title: string;
  body: string;
  /** The last passcode was wrong. */
  retry: boolean;
  /** An answer is on its way: OK waits. */
  sending?: boolean;
  onSubmit: (digits: string) => void;
  onCancel: () => void;
  /** "stage": over the Remote Play picture (absolute); "screen": over the whole app. */
  placement?: "stage" | "screen";
  testId?: string;
}) {
  const tr = useTr();
  const [value, setValue] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const ok = isPasscode(value) && !sending;

  const cancelRef = useRef(onCancel);
  useEffect(() => {
    cancelRef.current = onCancel;
  });

  useEffect(() => {
    input.current?.focus({ preventScroll: true });
    // Esc cancels, focused or not, before anything else's own Esc handling.
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
    };
  }, []);

  return (
    <div
      className={`pointer-events-auto ${placement === "stage" ? "absolute z-40" : "fixed z-[1000]"} inset-0 flex items-center justify-center bg-black/60 p-4`}
      {...keep}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="flex w-[min(22rem,100%)] flex-col gap-3 rounded-2xl border border-white/10 bg-neutral-900/95 p-4 text-white shadow-2xl backdrop-blur-md"
        data-testid={testId}
      >
        <div className="flex items-center gap-2 text-sm font-medium">
          <LockKeyhole size={16} aria-hidden />
          {title}
        </div>
        <p className="text-xs text-white/70">{body}</p>
        {retry && (
          <p className="text-xs font-medium text-[var(--color-danger,#f87171)]" role="alert" data-testid={`${testId}-wrong`}>
            {tr("rp_passcode_wrong", undefined, "Wrong passcode, try again.")}
          </p>
        )}
        <form
          className="flex items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (ok) onSubmit(value);
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
            data-testid={`${testId}-input`}
            onChange={(e) => setValue(e.target.value.replace(/\D/g, "").slice(0, 4))}
          />
          <button
            type="submit"
            disabled={!ok}
            className="rounded-full bg-white px-4 py-2 text-sm font-medium text-black focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] disabled:opacity-40"
            data-testid={`${testId}-ok`}
          >
            {tr("rp_passcode_ok", undefined, "OK")}
          </button>
          <button
            type="button"
            className="rounded-full bg-white/15 px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
            onClick={onCancel}
            data-testid={`${testId}-cancel`}
          >
            {tr("rp_keyboard_cancel", undefined, "Cancel")}
          </button>
        </form>
      </div>
    </div>
  );
}
