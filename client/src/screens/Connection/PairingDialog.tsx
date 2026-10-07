import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { Button } from "../../components";
import { Modal } from "../../components/Modal";
import { useTr } from "../../state/lang";
import { usePairingStore } from "../../state/pairing";
import { sendHelperTo } from "../../state/helperSendRuntime";
import { isTauriEnv } from "../../lib/tauriEnv";
import type { PairingView } from "../../api/ava1";

export interface PairingPanelProps {
  view: PairingView | null;
  busy: boolean;
  error: string | null;
  /** The six digits the user typed from the console's screen. */
  onConfirm: (code: string) => void;
  onRetry: () => void;
  onCancel: () => void;
  /** "Forget the old one and pair this one" (a different PS5 answered at the address). */
  onForget?: () => void;
  /** Send the helper again, then pair: a helper this app sends pairs by itself. Absent where
   *  the app cannot send a helper (the browser build). */
  onResend?: () => void;
}

/** The body and buttons of the pairing dialog, separate from the store so it renders (and
 *  tests) without one. Passkey entry: the console shows a six-digit code on its own screen
 *  and the user types it here; the app never displays a code. Situations: a code to enter
 *  (or re-enter after a wrong one), a closed pairing window, a different console at a
 *  pinned address, and a console that could not be reached. */
export function PairingPanel({
  view,
  busy,
  error,
  onConfirm,
  onRetry,
  onCancel,
  onForget,
  onResend,
}: PairingPanelProps) {
  const tr = useTr();
  const [digits, setDigits] = useState("");
  const wrong = view?.state === "wrong_code";
  useEffect(() => {
    // A refused code is cleared so the next attempt starts from an empty field.
    if (wrong) setDigits("");
  }, [wrong, busy]);
  const ready = digits.length === 6;
  const submit = () => {
    if (ready && !busy) onConfirm(digits);
  };

  if (view?.state === "code" || view?.state === "wrong_code") {
    const name = view.consoleName || "PS5";
    const label = tr(
      "pairing_enter_label",
      undefined,
      "Enter the code shown on your PS5",
    );
    return (
      <>
        <div className="flex flex-col gap-3 p-4 text-sm">
          <p>
            {tr(
              "pairing_enter_intro",
              { name },
              `${name} is asking to pair with this app. Enter the code shown on your PS5.`,
            )}
          </p>
          <label className="flex flex-col items-center gap-1">
            <span className="text-xs text-[var(--color-muted)]">{label}</span>
            <input
              className="w-48 rounded-md border border-[var(--color-border)] bg-transparent px-3 py-2 text-center font-mono text-2xl font-semibold tracking-[0.3em] tabular-nums"
              data-testid="pairing-code-input"
              type="text"
              inputMode="numeric"
              pattern="[0-9]*"
              autoComplete="one-time-code"
              maxLength={6}
              autoFocus
              aria-label={label}
              value={digits}
              onChange={(e) =>
                setDigits(e.target.value.replace(/\D/g, "").slice(0, 6))
              }
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
              }}
            />
          </label>
          {wrong && (
            <p className="text-xs text-[var(--color-bad)]" role="alert">
              {tr(
                "pairing_wrong_code",
                undefined,
                "That code didn't match. The PS5 now shows a new code: enter that one.",
              )}
            </p>
          )}
          {error && (
            <p className="text-xs text-[var(--color-bad)]" role="alert">
              {tr("pairing_error", { error }, `Pairing failed: ${error}`)}
            </p>
          )}
        </div>
        <div className="flex items-center justify-end gap-2 border-t border-[var(--color-border)] px-4 py-3">
          <Button variant="ghost" onClick={onCancel}>
            {tr("close", undefined, "Close")}
          </Button>
          <Button
            variant="primary"
            loading={busy}
            disabled={!ready}
            onClick={submit}
          >
            {tr("pairing_enter_submit", undefined, "Pair")}
          </Button>
        </div>
      </>
    );
  }

  if (view?.state === "wrong_console") {
    return (
      <>
        <div className="flex flex-col gap-2 p-4 text-sm">
          <p className="font-medium">
            {tr(
              "pairing_wrong_console_title",
              undefined,
              "A different PS5 answered at this address",
            )}
          </p>
          <p className="text-[var(--color-muted)]">
            {tr(
              "pairing_wrong_console_body",
              undefined,
              "This app remembers another PS5 at this address, for example after the address changed hands. If you replaced or swapped consoles, forget the old one and pair this one.",
            )}
          </p>
          {error && (
            <p className="text-xs text-[var(--color-bad)]" role="alert">
              {tr("pairing_error", { error }, `Pairing failed: ${error}`)}
            </p>
          )}
        </div>
        <div className="flex items-center justify-end gap-2 border-t border-[var(--color-border)] px-4 py-3">
          <Button variant="ghost" onClick={onCancel}>
            {tr("close", undefined, "Close")}
          </Button>
          <Button variant="primary" loading={busy} onClick={onForget}>
            {tr(
              "pairing_forget_and_pair",
              undefined,
              "Forget the old one and pair this one",
            )}
          </Button>
        </div>
      </>
    );
  }

  if (view?.state === "closed") {
    return (
      <>
        <div className="flex flex-col gap-2 p-4 text-sm">
          <p className="font-medium">
            {tr(
              "pairing_closed_title",
              undefined,
              "The PS5 is not accepting new pairings",
            )}
          </p>
          <p className="text-[var(--color-muted)]">
            {tr(
              "pairing_closed_body",
              undefined,
              "Its pairing window is closed. On a device that is already paired, open pairing for this console. If no device is paired yet, restart the helper on the console to reopen the window. Then try again.",
            )}
          </p>
        </div>
        <div className="flex items-center justify-end gap-2 border-t border-[var(--color-border)] px-4 py-3">
          <Button variant="ghost" onClick={onCancel}>
            {tr("close", undefined, "Close")}
          </Button>
          {onResend && (
            <Button
              variant="secondary"
              loading={busy}
              onClick={onResend}
              data-testid="pairing-resend-helper"
            >
              {tr("connection_send_resend", undefined, "Resend helper")}
            </Button>
          )}
          <Button variant="primary" loading={busy} onClick={onRetry}>
            {tr("pairing_retry", undefined, "Try again")}
          </Button>
        </div>
      </>
    );
  }

  // Loading, or the console could not be reached.
  return (
    <>
      <div className="flex flex-col gap-2 p-4 text-sm">
        {error ? (
          <p className="text-[var(--color-bad)]" role="alert">
            {tr("pairing_error", { error }, `Pairing failed: ${error}`)}
          </p>
        ) : (
          <p className="flex items-center gap-2 text-[var(--color-muted)]">
            <Loader2 size={14} className="animate-spin" aria-hidden />
            {tr("pairing_waiting", undefined, "Contacting the PS5…")}
          </p>
        )}
      </div>
      <div className="flex items-center justify-end gap-2 border-t border-[var(--color-border)] px-4 py-3">
        <Button variant="ghost" onClick={onCancel}>
          {tr("close", undefined, "Close")}
        </Button>
        {error && (
          <Button variant="primary" loading={busy} onClick={onRetry}>
            {tr("pairing_retry", undefined, "Try again")}
          </Button>
        )}
      </div>
    </>
  );
}

/** The dialog the store opens: when any call comes back not_paired, or from the Pair… button.
 *  Mounted once, in the app shell. The same engine routes serve the desktop, browser and
 *  Docker builds. */
export function PairingDialog() {
  const tr = useTr();
  const open = usePairingStore((s) => s.open);
  const view = usePairingStore((s) => s.view);
  const busy = usePairingStore((s) => s.busy);
  const error = usePairingStore((s) => s.error);
  const confirm = usePairingStore((s) => s.confirm);
  const forgetAndPair = usePairingStore((s) => s.forgetAndPair);
  const retry = usePairingStore((s) => s.retry);
  const dismiss = usePairingStore((s) => s.dismiss);
  const host = usePairingStore((s) => s.host);
  const [sending, setSending] = useState(false);
  // The quickest way out of a closed pairing window: a helper this app sends pairs by itself,
  // so send it and ask again. Only where the app can send one.
  const resend =
    isTauriEnv() && host
      ? async () => {
          setSending(true);
          try {
            await sendHelperTo(host, tr);
          } finally {
            setSending(false);
          }
          await retry();
        }
      : undefined;
  return (
    <Modal
      open={open}
      onClose={dismiss}
      title={tr("pairing_title", undefined, "Pair with your PS5")}
      size="sm"
      closeOnScrim={false}
    >
      <PairingPanel
        view={view}
        busy={busy || sending}
        error={error}
        onResend={resend ? () => void resend() : undefined}
        onConfirm={(code) => void confirm(code)}
        onForget={() => void forgetAndPair()}
        onRetry={() => void retry()}
        onCancel={dismiss}
      />
    </Modal>
  );
}
