import { useCallback, useEffect, useRef, useState } from "react";

import {
  cheatsEngineSet,
  cheatsGet,
  cheatsStatus,
  cheatsToggle,
  type CheatMod,
} from "../../../api/ps5";
import { transferAddr } from "../../../lib/addr";
import { afterToggle, anyWaiting } from "../../Cheats/cheatApplied";

/** How often the Game panel reads what runs, while it is open (nothing while closed). */
export const GAME_POLL_MS = 3000;

/** The running game as the helper sees it, and its cheats: what the Game panel shows. */
export interface StreamGame {
  /** Read at least once. */
  ready: boolean;
  /** The last read failed (the helper is not up, say); the panel says so. */
  error: string | null;
  /** The running title, or null when none runs. */
  titleId: string | null;
  /** The last title seen running while the panel was open: tells "it closed" from "none ran". */
  seen: string | null;
  /** The helper's cheat engine. */
  engine: boolean;
  /** The helper paused this game's cheats: it closed right after they were applied (#550). */
  held: boolean;
  /** The running title's cheats; null while not read. */
  mods: CheatMod[] | null;
  modsError: string | null;
  /** The index of the cheat being switched. */
  busy: number | null;
  engineBusy: boolean;
  /** The last switch's failure, one line. */
  message: string | null;
}

const EMPTY: StreamGame = {
  ready: false,
  error: null,
  titleId: null,
  seen: null,
  engine: false,
  held: false,
  mods: null,
  modsError: null,
  busy: null,
  engineBusy: false,
  message: null,
};

const text = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** What runs on `host` (the streamed console) and its cheats, read every few seconds while `on`.
 *  One poller for every tab of the panel. */
export function useStreamGame(host: string | null, on: boolean) {
  const [s, setS] = useState<StreamGame>(EMPTY);
  // The state as of the last change, for callbacks (the rendered copy lags a render behind).
  const ref = useRef<StreamGame>(EMPTY);
  const addr = host ? transferAddr(host) : "";
  /** Every list read gets a number; only the newest answer is kept. */
  const listSeq = useRef(0);

  const patch = useCallback((p: Partial<StreamGame>) => {
    ref.current = { ...ref.current, ...p };
    setS(ref.current);
  }, []);

  const loadMods = useCallback(
    async (title: string) => {
      const seq = ++listSeq.current;
      try {
        const r = await cheatsGet(title, addr);
        if (seq !== listSeq.current || ref.current.titleId !== title) return;
        patch({ mods: r.mods ?? [], modsError: r.error ?? null });
      } catch (e) {
        if (seq !== listSeq.current || ref.current.titleId !== title) return;
        patch({ modsError: text(e) });
      }
    },
    [addr, patch],
  );

  const read = useCallback(async () => {
    if (!addr) return;
    try {
      const st = await cheatsStatus(addr);
      const title = st.game_running && st.game_title_id ? st.game_title_id : null;
      const before = ref.current.titleId;
      patch({
        ready: true,
        error: null,
        titleId: title,
        seen: title ?? ref.current.seen,
        engine: st.enabled,
        held: !!(title && st.held),
        // Another game (or none): never show the last one's cheats.
        ...(title !== before ? { mods: null, modsError: null, message: null } : {}),
      });
      // A cheat still waiting for the game to load (#627): read the list again to show it apply.
      if (title && (title !== before || ref.current.mods === null || anyWaiting(ref.current.mods)))
        void loadMods(title);
    } catch (e) {
      patch({ ready: true, error: text(e) });
    }
  }, [addr, loadMods, patch]);

  useEffect(() => {
    if (!on || !addr) return;
    void read();
    const t = window.setInterval(() => void read(), GAME_POLL_MS);
    return () => window.clearInterval(t);
  }, [on, addr, read]);

  // Another console: start over.
  useEffect(() => {
    ref.current = EMPTY;
    setS(EMPTY);
  }, [addr]);

  const toggle = useCallback(
    async (m: CheatMod) => {
      const title = ref.current.titleId;
      if (!title || ref.current.busy !== null) return;
      patch({ busy: m.index, message: null });
      try {
        const r = await cheatsToggle(title, m.index, !m.on, addr);
        if (!r.ok) {
          patch({ message: r.err || null });
          return;
        }
        patch({
          mods: ref.current.mods?.map((x) => (x.index === m.index ? afterToggle(x, !m.on) : x)) ?? null,
          // Switching one on turns the engine on (the helper does it) and resumes a held game.
          ...(!m.on ? { engine: true, held: false } : {}),
        });
      } catch (e) {
        patch({ message: text(e) });
      } finally {
        patch({ busy: null });
      }
    },
    [addr, patch],
  );

  const setEngine = useCallback(
    async (enabled: boolean) => {
      patch({ engineBusy: true, message: null });
      try {
        await cheatsEngineSet(enabled, addr);
        patch({ engine: enabled });
      } catch (e) {
        patch({ message: text(e) });
      } finally {
        patch({ engineBusy: false });
      }
    },
    [addr, patch],
  );

  return { game: s, toggle, setEngine, refresh: read };
}
