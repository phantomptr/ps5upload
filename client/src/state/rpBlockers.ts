// Remote Play blockers around a stream (lib/rpBlockers.ts): checked before a stream starts,
// waited out when one gets out of the way by itself (Hush's hold mode), paused for it when the
// user says so (or always, by the setting), and turned back on when the stream ends.

import { create } from "zustand";

import {
  RP_BLOCKERS,
  blockerById,
  blockerCtx,
  checkBlockers,
  type BlockerStatus,
  type RpBlocker,
} from "../lib/rpBlockers";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { trStatic } from "../lib/trStatic";
import { log } from "./logs";
import { pushNotification } from "./notifications";
import { useRpQualityStore } from "./rpQuality";
import { useRemotePlayStore } from "./remotePlay";

const AUTO_PAUSE_KEY = "ps5upload.rp_pause_blockers";

/** A blocker a pause cannot help, and what clears it (i18n key and English). */
export interface StuckBlocker {
  id: string;
  key: string;
  text: string;
}

/** A start held back: what is on (pausable, and stuck), and the stream start to run once the
 *  user chose. */
interface Held {
  host: string;
  ids: string[];
  stuck: StuckBlocker[];
  go: () => Promise<void>;
}

interface RpBlockersState {
  /** Always pause a blocker for Remote Play, without asking (off by default). */
  autoPause: boolean;
  setAutoPause: (on: boolean) => void;
  /** A start waiting on the user's choice. */
  held: Held | null;
  /** A start is looking for blockers on this console (a sleeping one answers nothing for 4 s). */
  checking: string | null;
  /** Blockers being paused before a stream (the start waits for them). */
  pausing: { host: string; ids: string[] } | null;
  /** Blockers a start waits out (they get out of the way by themselves, within `maxS`). */
  waiting: { host: string; ids: string[]; maxS: number; cancel: () => void } | null;
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
  checking: null,
  pausing: null,
  waiting: null,
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

type Found = { blocker: RpBlocker; status: Exclude<BlockerStatus, { kind: "off" }> };

/**
 * Before a stream starts on `host`: look for blockers, wait out the ones that get out of the
 * way by themselves, then start (`go`) when none is left, pause them first when the setting
 * says always, or hold the start for the user's choice. A blocker a pause cannot help always
 * holds the start, with what clears it.
 */
export async function beforeRemotePlay(
  host: string,
  go: () => Promise<void>,
  list: readonly RpBlocker[] = RP_BLOCKERS,
): Promise<void> {
  const k = key(host);
  // A pause or wait in progress starts the stream itself when it is done.
  const s0 = useRpBlockers.getState();
  if (s0.pausing || s0.waiting) return;
  const ctx = blockerCtx(host);
  if (s0.checking) return;
  useRpBlockers.setState({ checking: k });
  let found = await withTimeout(checkBlockers(ctx, list).catch(() => [] as Found[]), CHECK_TIMEOUT_MS, [] as Found[]);
  useRpBlockers.setState({ checking: null });
  const waits = found.filter((f) => f.status.kind === "wait");
  if (waits.length) {
    found = found.filter((f) => f.status.kind !== "wait");
    const stillOn = await waitOut(host, waits);
    if (stillOn === null) return;
    found.push(...stillOn);
  }
  const ids = found.filter((f) => f.status.kind === "on").map((f) => f.blocker.id);
  const stuck = found.flatMap((f) =>
    f.status.kind === "stuck" ? [{ id: f.blocker.id, key: f.status.key, text: f.status.text }] : [],
  );
  const all = found.map((f) => f.blocker.id);
  useRpBlockers.setState((s) => ({ onAtStart: { ...s.onAtStart, [k]: all }, held: null }));
  if (all.length === 0) return go();
  log.info("remote-play", `blockers on ${k}: ${all.join(", ")}${stuck.length ? ` (stuck: ${stuck.map((x) => x.id).join(", ")})` : ""}`);
  if (useRpBlockers.getState().autoPause && stuck.length === 0) return pauseThenStart(host, ids, go);
  useRpBlockers.setState({ held: { host, ids, stuck, go } });
}

/** Wait for these blockers to get out of the way by themselves. Resolves with the ones still
 *  in the way when their time ran out (they count as on from then), or null when the user
 *  cancelled the start. */
async function waitOut(host: string, waits: readonly Found[]): Promise<Found[] | null> {
  const k = key(host);
  const ctx = blockerCtx(host);
  const ac = new AbortController();
  const maxMs = Math.max(...waits.map((w) => (w.status.kind === "wait" ? w.status.maxMs : 0)));
  const ids = waits.map((w) => w.blocker.id);
  useRpBlockers.setState({ waiting: { host, ids, maxS: Math.ceil(maxMs / 1000), cancel: () => ac.abort() } });
  log.info("remote-play", `waiting for ${ids.join(", ")} on ${k} (up to ${Math.ceil(maxMs / 1000)} s)`);
  try {
    const outcomes = await Promise.all(
      waits.map((w) =>
        (w.blocker.waitClear?.(ctx, w.status.kind === "wait" ? w.status.maxMs : 0, { signal: ac.signal }) ??
          Promise.resolve("timeout" as const)
        ).catch(() => "timeout" as const),
      ),
    );
    if (ac.signal.aborted) {
      log.info("remote-play", `start on ${k} cancelled while waiting for ${ids.join(", ")}`);
      return null;
    }
    const left = waits.filter((_, i) => outcomes[i] !== "clear");
    for (const w of waits) {
      log.info("remote-play", `${w.blocker.name} on ${k}: ${left.includes(w) ? "still in the way" : "out of the way"}`);
    }
    return left.map((w) => ({ blocker: w.blocker, status: { kind: "on" } as const }));
  } finally {
    useRpBlockers.setState({ waiting: null });
  }
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

/** The console a restart with other settings left without a stream for now. */
let restartingHost: string | null = null;

// When a stream ends, what was paused for it goes back on.
useRemotePlayStore.subscribe((s, prev) => {
  if (!prev.stream || s.stream || !prev.target) return;
  // A restart with other settings (720p, Go live's): the next stream still needs them paused.
  if (useRpQualityStore.getState().switching) {
    restartingHost = prev.target.host;
    return;
  }
  const host = prev.target.host;
  if (useRpBlockers.getState().paused[key(host)]?.length) void resumePaused(host);
});

// That restart called off, or failed: nothing is streaming now.
useRpQualityStore.subscribe((s, prev) => {
  if (!prev.switching || s.switching || !restartingHost) return;
  const host = restartingHost;
  restartingHost = null;
  if (!useRemotePlayStore.getState().stream && useRpBlockers.getState().paused[key(host)]?.length) void resumePaused(host);
});
