// The pairing dialog's state. The dialog opens when any call comes back `not_paired`
// (via `reportIfNotPaired`, wired in lib/invokeLogged and the upload poll) or when the user
// presses Pair… on the status pill. The engine holds the handshake whose code is on screen;
// this store relays the six digits the user types from the console's screen (passkey entry:
// the console checks them, the app never shows a code).
//
// The app's own launches never reach it: a helper this app sent pairs with no code.

import { create } from "zustand";

import {
  pairingCancel,
  pairingConfirm,
  pairingForget,
  pairingStatus,
  type PairingView,
} from "../api/ava1";
import { hostOf } from "../lib/addr";
import { onNotPaired } from "../lib/consoleSession";
import { useConnectionStore } from "./connection";

/** After the user dismisses the dialog for a console, an automatic open for it waits this long. */
const QUIET_MS = 60_000;

const UNKNOWN_STATE = "unexpected reply from the engine";

interface PairingState {
  open: boolean;
  host: string;
  /** The last thing the engine said; null while loading or after a transport error. */
  view: PairingView | null;
  busy: boolean;
  error: string | null;
  /** The console that was just paired (the status poll flips its pill on its next tick). */
  paired: string | null;
  /** Per console: no automatic open before this wall-clock ms. */
  quietUntil: Record<string, number>;
  /** Opens for `host` (default: the active console) and starts/re-reads the handshake. */
  openFor: (host?: string) => Promise<void>;
  /** Asks again (after the user opened the pairing window on the console). */
  retry: () => Promise<void>;
  /** Asks again after the helper was sent again, and keeps asking for a short while if the
   *  window still reads closed: the new helper pairs with the app that sent it, but a moment
   *  after it starts. Measured from a phone: closed at once, paired seconds later, and the
   *  dialog was left explaining a closed window to a console that had already accepted. */
  retryAfterResend: (sleep?: (ms: number) => Promise<void>) => Promise<void>;
  /** Sends the six digits the user typed. */
  confirm: (code: string) => Promise<void>;
  /** "Forget the old one and pair this one": clears the pinned key, then pairs afresh. */
  forgetAndPair: () => Promise<void>;
  /** Closes the dialog. */
  close: () => void;
  /** Closes it because the user said no: automatic opens for this console go quiet. */
  dismiss: () => void;
}

/** How often, and how far apart, a closed window is asked again after a resend (about 16 s). */
const RESEND_ASKS = 8;
const RESEND_ASK_GAP_MS = 2000;

export const usePairingStore = create<PairingState>((set, get) => {
  const load = async (host: string) => {
    set({ busy: true, error: null });
    try {
      const view = await pairingStatus(host);
      // Already paired (the console trusts us after all): nothing to compare.
      if (view.state === "accepted") {
        set({ busy: false, view, open: false, paired: hostOf(host) });
        return;
      }
      if (view.state === "none") {
        // The engine said something this build does not know: an error with Retry, never a
        // panel stuck on "Contacting the PS5…".
        set({ busy: false, view: null, error: UNKNOWN_STATE });
        return;
      }
      set({ busy: false, view });
    } catch (e) {
      set({
        busy: false,
        view: null,
        error: e instanceof Error ? e.message : String(e),
      });
    }
  };

  return {
    open: false,
    host: "",
    view: null,
    busy: false,
    error: null,
    paired: null,
    quietUntil: {},

    async openFor(host) {
      const h = (host ?? useConnectionStore.getState().host).trim();
      if (!h) return;
      set({ open: true, host: h, view: null, error: null, paired: null });
      await load(h);
    },

    async retry() {
      const { host } = get();
      if (host) await load(host);
    },

    async retryAfterResend(
      sleep = (ms) => new Promise<void>((r) => setTimeout(r, ms)),
    ) {
      const { host } = get();
      if (!host) return;
      for (let attempt = 0; attempt < RESEND_ASKS; attempt++) {
        if (attempt > 0) await sleep(RESEND_ASK_GAP_MS);
        // The user closed the dialog, or it moved to another console, while this waited.
        if (!get().open || get().host !== host) return;
        await load(host);
        if (get().view?.state !== "closed") return;
      }
    },

    async confirm(code) {
      const { host, view } = get();
      if (!host || (view?.state !== "code" && view?.state !== "wrong_code"))
        return;
      set({ busy: true, error: null });
      try {
        const next = await pairingConfirm(host, code);
        if (next.state === "accepted") {
          set({ busy: false, open: false, view: next, paired: hostOf(host) });
        } else if (next.state === "none") {
          set({ busy: false, error: UNKNOWN_STATE });
        } else {
          // `wrong_code` (type again) or `closed` (five wrong codes, or the window shut).
          set({ busy: false, view: next });
        }
      } catch (e) {
        set({
          busy: false,
          error: e instanceof Error ? e.message : String(e),
        });
      }
    },

    async forgetAndPair() {
      const { host } = get();
      if (!host) return;
      set({ busy: true, error: null });
      try {
        await pairingForget(host);
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
        return;
      }
      await load(host);
    },

    close() {
      set({ open: false, view: null, error: null, busy: false });
    },

    dismiss() {
      const host = get().host;
      // The engine's pending handshake holds one of the console's two unconfirmed places;
      // dismissing the dialog hands it back (best effort: it also expires on its own).
      if (host) void pairingCancel(host);
      const key = hostOf(host) || "_";
      set((s) => ({
        open: false,
        view: null,
        error: null,
        busy: false,
        quietUntil: { ...s.quietUntil, [key]: Date.now() + QUIET_MS },
      }));
    },
  };
});

// Any call that comes back not_paired opens the dialog, unless it is already open or
// the user just dismissed it for this console.
onNotPaired((host) => {
  const s = usePairingStore.getState();
  if (s.open) return;
  // A failure that names no console says nothing about which one to pair: never guess the
  // active console (console B's failure must not open A's dialog).
  const h = (host ?? "").trim();
  if (!h) return;
  const key = hostOf(h) || "_";
  if (Date.now() < (s.quietUntil[key] ?? 0)) return;
  void s.openFor(h);
});
