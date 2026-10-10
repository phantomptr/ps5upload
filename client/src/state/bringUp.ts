import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { PS5_LOADER_PORT } from "./connection";
import { sendHelperAndWait } from "./helperSendRuntime";
import type { Translator } from "./lang";
import { log } from "./logs";
import {
  runStatusForHost,
  usePayloadPlaylistsStore,
} from "./payloadPlaylists";

/**
 * Quick bring-up — one-tap cold-boot of a PS5 to a usable state.
 *
 * Collapses the manual post-boot ritual (send kernel-R/W payload → send SMP →
 * send the helper → wait for it → send my apps) into a single action by
 * composing pieces that already exist:
 *
 *   phase "prehelper" — run the configured BRING-UP PLAYLIST (kstuff, SMP, …)
 *                       against the loader (:9021). Optional; skipped if none.
 *   phase "helper"    — send the helper and wait for it to answer, through the same
 *                       sendHelperTo as Connection's Send helper: it joins a send already
 *                       under way and opens pairing for an unpaired console.
 *   (then the post-helper AUTO-LOADER playlist runs.)
 *
 * Status is kept per console (by bare host): each console's Connection screen shows its own, and
 * bringing up one console neither shows on nor blocks another's.
 */

export type BringUpPhase = "prehelper" | "helper";

export type BringUpStatus =
  | { kind: "idle" }
  | { kind: "running"; host: string; phase: BringUpPhase; detail: string }
  | { kind: "done"; host: string }
  | { kind: "failed"; host: string; phase: BringUpPhase; error: string };

interface BringUpState {
  /** Keyed by bare host. A console with no entry is idle. */
  byHost: Record<string, BringUpStatus>;
  /** Run the full bring-up chain against `host` (a bare ip or ip:port). */
  run: (host: string, tr: Translator) => Promise<void>;
  reset: (host: string) => void;
}

const IDLE: BringUpStatus = { kind: "idle" };

/** `host`'s bring-up status (bare ip or ip:port). */
export function bringUpStatusFor(s: Pick<BringUpState, "byHost">, host: string): BringUpStatus {
  return s.byHost[hostOf(host.trim())] ?? IDLE;
}

export const useBringUpStore = create<BringUpState>((set, get) => ({
  byHost: {},
  reset: (host) =>
    set((s) => {
      const next = { ...s.byHost };
      delete next[hostOf(host.trim())];
      return { byHost: next };
    }),

  async run(host, tr) {
    const h = host.trim();
    if (!h) return;
    const bare = hostOf(h);
    if (bringUpStatusFor(get(), bare).kind === "running") return; // already bringing this one up
    const put = (status: BringUpStatus) => set((s) => ({ byHost: { ...s.byHost, [bare]: status } }));
    let phase: BringUpPhase = "prehelper";
    try {
      // ── Phase 1: pre-helper bring-up playlist (kstuff / SMP / …) ──────────
      const pl = usePayloadPlaylistsStore.getState();
      const id = pl.autoLoader.bringUpPlaylistId;
      const playlist = id ? pl.playlists.find((p) => p.id === id) : undefined;
      if (playlist && playlist.steps.length > 0) {
        put({ kind: "running", host: bare, phase, detail: playlist.name });
        // run() resolves when the whole playlist (incl. sleeps) finishes; it
        // records failure in per-host run status rather than throwing.
        await pl.run(playlist.id, h, PS5_LOADER_PORT);
        const rs = runStatusForHost(usePayloadPlaylistsStore.getState(), h);
        if (rs.kind === "failed") {
          const step = "stepIndex" in rs ? rs.stepIndex + 1 : 0;
          const err = "error" in rs ? rs.error : "unknown error";
          throw new Error(`pre-helper step ${step} failed: ${err}`);
        }
      }

      // ── Phase 2: send the helper and wait for it to answer ───────────────
      phase = "helper";
      put({ kind: "running", host: bare, phase, detail: "" });
      const failure = await sendHelperAndWait(h, tr);
      if (failure !== null) throw new Error(failure);

      // Run the post-helper auto-loader playlist OURSELVES. We can't rely on
      // AppShell's auto-loader edge here: it only fires on a down→up
      // transition, but on a COLD console the helper comes up as the first
      // known state (prev = "unknown"), so that edge is suppressed — the exact
      // case bring-up is for. The playlist store's per-host run() guard makes
      // this safe even if the edge somehow also fires (it won't double-run).
      const plg = usePayloadPlaylistsStore.getState();
      const auto = plg.autoLoader;
      const post = auto.playlistId
        ? plg.playlists.find((p) => p.id === auto.playlistId)
        : undefined;
      if (auto.enabled && post && post.steps.length > 0) {
        log.info(
          "connection",
          `bring-up: running auto-loader "${post.name}" on ${h}`,
        );
        void plg.run(post.id, h, PS5_LOADER_PORT);
      }

      put({ kind: "done", host: bare });
      log.info("connection", `bring-up complete on ${h}`);
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      put({ kind: "failed", host: bare, phase, error });
      log.warn("connection", `bring-up failed on ${h} (${phase}): ${error}`);
    }
  },
}));
