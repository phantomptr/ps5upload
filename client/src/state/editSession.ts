// The one image currently checked out of ShadowMount+ for editing.
//
// While a checkout is open the image is NOT where ShadowMount+ can see it, so
// the game is missing from the PS5 home screen until the session is finished.
// That makes the session something the user has to be able to find again —
// including after closing the app, or from a different machine — so the
// authoritative record lives on the console (see
// `ps5upload_core::smp_checkout`) and this store is only a cache of it.
//
// Per-console, like the library store: two consoles can each have their own
// edit session, and showing one console's banner while looking at the other
// would invite finishing the wrong one.

import { create } from "zustand";

import {
  smpCheckoutFinish,
  smpCheckoutStatus,
  smpImageRwFinish,
  smpImageRwStatus,
  type SmpImageRwSession,
  type SmpCheckout,
} from "../api/ps5";
import { hostOf } from "../lib/addr";

interface EditSessionSlot {
  checkout: SmpCheckout | null;
  imageRw: SmpImageRwSession | null;
  /** True while a refresh or finish is in flight. */
  busy: boolean;
  error: string | null;
  /** null = never probed. Distinguishes "no session" from "don't know yet",
   *  so the banner doesn't flash absent before the first probe lands. */
  lastProbedAt: number | null;
}

const IDLE: EditSessionSlot = {
  checkout: null,
  imageRw: null,
  busy: false,
  error: null,
  lastProbedAt: null,
};

interface EditSessionStore {
  byHost: Record<string, EditSessionSlot>;
  refresh: (host: string) => Promise<void>;
  finish: (host: string) => Promise<SmpCheckout | SmpImageRwSession | null>;
  clearError: (host: string) => void;
}

const keyOf = (host: string | null | undefined): string =>
  host?.trim() ? hostOf(host) : "";

export function editSessionForHost(
  s: { byHost: Record<string, EditSessionSlot> },
  host: string | null | undefined,
): EditSessionSlot {
  return s.byHost[keyOf(host)] ?? IDLE;
}

export const useEditSessionStore = create<EditSessionStore>((set, get) => {
  const patch = (host: string, partial: Partial<EditSessionSlot>) =>
    set((s) => {
      const key = keyOf(host);
      const cur = s.byHost[key] ?? IDLE;
      return { byHost: { ...s.byHost, [key]: { ...cur, ...partial } } };
    });

  return {
    byHost: {},

    refresh: async (host) => {
      if (!host?.trim()) return;
      patch(host, { busy: true });
      try {
        const [checkout, imageRw] = await Promise.all([
          smpCheckoutStatus(host),
          smpImageRwStatus(host),
        ]);
        patch(host, {
          checkout,
          imageRw,
          busy: false,
          error: null,
          lastProbedAt: Date.now(),
        });
      } catch (e) {
        // A probe failure must NOT read as "no session open" — that would
        // hide a checked-out image and leave it stranded. Keep whatever we
        // last knew and surface the error instead.
        patch(host, {
          busy: false,
          error: e instanceof Error ? e.message : String(e),
        });
      }
    },

    finish: async (host) => {
      if (!host?.trim()) return null;
      patch(host, { busy: true, error: null });
      try {
        const slot = editSessionForHost(get(), host);
        const done = slot.imageRw
          ? await smpImageRwFinish(host)
          : slot.checkout
            ? await smpCheckoutFinish(host)
            : null;
        patch(host, {
          checkout: null,
          imageRw: null,
          busy: false,
          error: null,
          lastProbedAt: Date.now(),
        });
        return done;
      } catch (e) {
        patch(host, {
          busy: false,
          error: e instanceof Error ? e.message : String(e),
        });
        // Re-probe: `finish` is several steps (unmount, move, clear journal)
        // and a failure partway leaves a state the cached copy no longer
        // describes.
        await get().refresh(host);
        throw e;
      }
    },

    clearError: (host) => patch(host, { error: null }),
  };
});
