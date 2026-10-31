// Remote Play blockers around a stream (lib/rpBlockers.ts): checked before a stream starts,
// paused for it when the user says so (or always, by the setting), and turned back on when
// the stream ends.

import { create } from "zustand";

import {
  RP_BLOCKERS,
  blockerById,
  blockerCtx,
  detectBlockers,
  type RpBlocker,
} from "../lib/rpBlockers";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { trStatic } from "../lib/trStatic";
import { log } from "./logs";
import { pushNotification } from "./notifications";
import { useRemotePlayStore } from "./remotePlay";

const AUTO_PAUSE_KEY = "ps5upload.rp_pause_blockers";

/** A start held back: what is on, and the stream start to run once the user chose. */
interface Held {
  host: string;
  ids: string[];
  go: () => Promise<void>;
}

interface RpBlockersState {
  /** Always pause a blocker for Remote Play, without asking (off by default). */
  autoPause: boolean;
  setAutoPause: (on: boolean) => void;
  /** A start waiting on the user's choice. */
  held: Held | null;
  /** Blockers being paused before a stream (the start waits for them). */
  pausing: { host: string; ids: string[] } | null;
  /** Per console (host): blockers this app paused for the stream now running. */
  paused: Record<string, string[]>;
  /** Per console: blockers that were on when its last stream started (paused or not), so a
   *  stream that fails can name the cause. */
  onAtStart: Record<string, string[]>;
}

function readAutoPause(): boolean {
  try {
    return safeGetItem(AUTO_PAUSE_KEY) === "1";
  } catch {
    return false;
  }
}

export const useRpBlockers = create<RpBlockersState>((set) => ({
  autoPause: readAutoPause(),
  setAutoPause(on) {
    safeSetItem(AUTO_PAUSE_KEY, on ? "1" : "0");
    set({ autoPause: on });
  },
  held: null,
  pausing: null,
  paused: {},
  onAtStart: {},
}));

/** How long the check may hold up a start: a console that is asleep or has no helper answers
 *  nothing, and Remote Play wakes it on its own. */
const CHECK_TIMEOUT_MS = 4000;

function withTimeout<T>(p: Promise<T>, ms: number, fallback: T): Promise<T> {
  return Promise.race([p, new Promise<T>((r) => setTimeout(() => r(fallback), ms))]);
}

const key = (host: string) => hostOf(host.trim()) || host.trim();

/** The names of these blockers, for messages. */
export function blockerNames(ids: readonly string[]): string {
  return ids.map((id) => blockerById(id)?.name ?? id).join(", ");
}

/**
 * Before a stream starts on `host`: look for blockers, then start (`go`) when there are none,
 * pause them first when the setting says always, or hold the start for the user's choice.
 */
export async function beforeRemotePlay(
  host: string,
  go: () => Promise<void>,
  list: readonly RpBlocker[] = RP_BLOCKERS,
): Promise<void> {
  const k = key(host);
  // A pause in progress starts the stream itself when it is done.
  if (useRpBlockers.getState().pausing) return;
  const found = await withTimeout(
    detectBlockers(blockerCtx(host), list).catch(() => []),
    CHECK_TIMEOUT_MS,
    [] as RpBlocker[],
  );
  const ids = found.map((b) => b.id);
  useRpBlockers.setState((s) => ({ onAtStart: { ...s.onAtStart, [k]: ids }, held: null }));
  if (ids.length === 0) return go();
  log.info("remote-play", `blockers on ${k}: ${ids.join(", ")}`);
  if (useRpBlockers.getState().autoPause) return pauseThenStart(host, ids, go);
  useRpBlockers.setState({ held: { host, ids, go } });
}

/** "Pause … while I play": pause what is on, then start. */
export async function pauseHeldAndStart(): Promise<void> {
  const h = useRpBlockers.getState().held;
  if (!h) return;
  useRpBlockers.setState({ held: null });
  await pauseThenStart(h.host, h.ids, h.go);
}

/** Start anyway, blockers and all. */
export async function startHeldAnyway(): Promise<void> {
  const h = useRpBlockers.getState().held;
  if (!h) return;
  useRpBlockers.setState({ held: null });
  await h.go();
}

export function dropHeld(): void {
  useRpBlockers.setState({ held: null });
}

async function pauseThenStart(host: string, ids: string[], go: () => Promise<void>): Promise<void> {
  const k = key(host);
  const ctx = blockerCtx(host);
  useRpBlockers.setState({ pausing: { host, ids } });
  const done: string[] = [];
  try {
    for (const id of ids) {
      const b = blockerById(id);
      if (!b) continue;
      try {
        const outcome = await b.pause(ctx);
        done.push(id);
        log.info("remote-play", `paused ${b.name} on ${k} (${outcome})`);
      } catch (e) {
        log.warn("remote-play", `could not pause ${b.name} on ${k}: ${String(e)}`);
        pushNotification(
          "warning",
          trStatic("rp_blocker_pause_failed", "Couldn't pause {name}. The stream may have no picture.").replace(
            "{name}",
            b.name,
          ),
          { body: String(e) },
        );
      }
    }
  } finally {
    useRpBlockers.setState((s) => ({ pausing: null, paused: { ...s.paused, [k]: done } }));
  }
  await go();
  // The start may not have become a stream (refused, already streaming elsewhere): nothing
  // will end, so turn them back on now.
  if (!streamingOn(k)) await resumePaused(host);
}

function streamingOn(k: string): boolean {
  const s = useRemotePlayStore.getState();
  return !!s.stream && !!s.target && key(s.target.host) === k;
}

/** Turn back on what this app paused for `host`'s stream. */
export async function resumePaused(host: string, ids?: readonly string[]): Promise<void> {
  const k = key(host);
  const which = ids ?? useRpBlockers.getState().paused[k] ?? [];
  useRpBlockers.setState((s) => {
    const paused = { ...s.paused };
    delete paused[k];
    return { paused };
  });
  const ctx = blockerCtx(host);
  for (const id of which) {
    const b = blockerById(id);
    if (!b) continue;
    try {
      const outcome = await b.resume(ctx);
      log.info("remote-play", `turned ${b.name} back on, ${k} (${outcome})`);
      if (outcome === "needs_send") {
        pushNotification(
          "info",
          trStatic("rp_blocker_resume_send", "{name} is set to on again. Send it to the PS5 to start it.").replace(
            "{name}",
            b.name,
          ),
        );
      }
    } catch (e) {
      log.warn("remote-play", `could not turn ${b.name} back on, ${k}: ${String(e)}`);
      pushNotification(
        "warning",
        trStatic("rp_blocker_resume_failed", "Couldn't turn {name} back on. Do it from its card in Payloads.").replace(
          "{name}",
          b.name,
        ),
        { body: String(e) },
      );
    }
  }
}

// When a stream ends, what was paused for it goes back on.
useRemotePlayStore.subscribe((s, prev) => {
  if (!prev.stream || s.stream || !prev.target) return;
  const host = prev.target.host;
  if (useRpBlockers.getState().paused[key(host)]?.length) void resumePaused(host);
});
