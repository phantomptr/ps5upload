import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { PS5_LOADER_PORT } from "./connection";
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
 *   phase "helper"    — send the app's bundled ps5upload helper to the loader.
 *   phase "waiting"   — poll until the helper reports ready.
 *   (then the AUTO-LOADER fires the post-helper playlist on the ready edge.)
 *
 * One console at a time — bring-up is a deliberate, foreground action driven
 * from the Connection screen, so a single global status is enough.
 */

export type BringUpPhase = "prehelper" | "helper" | "waiting";

export type BringUpStatus =
  | { kind: "idle" }
  | { kind: "running"; host: string; phase: BringUpPhase; detail: string }
  | { kind: "done"; host: string }
  | { kind: "failed"; host: string; phase: BringUpPhase; error: string };

interface BringUpState {
  status: BringUpStatus;
  /** Run the full bring-up chain against `host` (a bare ip or ip:port). */
  run: (host: string) => Promise<void>;
  reset: () => void;
}

/** Helper-ready poll: ~25s total (the helper's ucred elevation + bind can take
 *  a few seconds after a fresh send, especially right after kstuff). */
const READY_POLL_ATTEMPTS = 25;
const READY_POLL_INTERVAL_MS = 1000;

export const useBringUpStore = create<BringUpState>((set, get) => ({
  status: { kind: "idle" },
  reset: () => set({ status: { kind: "idle" } }),

  async run(host) {
    const h = host.trim();
    if (!h) return;
    if (get().status.kind === "running") return; // already bringing one up
    const bare = hostOf(h);
    let phase: BringUpPhase = "prehelper";
    try {
      // ── Phase 1: pre-helper bring-up playlist (kstuff / SMP / …) ──────────
      const pl = usePayloadPlaylistsStore.getState();
      const id = pl.autoLoader.bringUpPlaylistId;
      const playlist = id ? pl.playlists.find((p) => p.id === id) : undefined;
      if (playlist && playlist.steps.length > 0) {
        set({
          status: { kind: "running", host: bare, phase, detail: playlist.name },
        });
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

      // ── Phase 2: send the bundled helper to the loader ───────────────────
      phase = "helper";
      set({ status: { kind: "running", host: bare, phase, detail: "" } });
      const elf = await bundledPayloadPath();
      await sendPayload(h, elf);

      // ── Phase 3: wait for the helper to come up ──────────────────────────
      phase = "waiting";
      set({ status: { kind: "running", host: bare, phase, detail: "" } });
      let ready = false;
      for (let i = 0; i < READY_POLL_ATTEMPTS; i++) {
        const s = await payloadCheck(h);
        if (s.reachable) {
          ready = true;
          break;
        }
        await new Promise<void>((r) =>
          window.setTimeout(r, READY_POLL_INTERVAL_MS),
        );
      }
      if (!ready) throw new Error("helper did not report ready in time");

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

      set({ status: { kind: "done", host: bare } });
      log.info("connection", `bring-up complete on ${h}`);
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      set({ status: { kind: "failed", host: bare, phase, error } });
      log.warn("connection", `bring-up failed on ${h} (${phase}): ${error}`);
    }
  },
}));
