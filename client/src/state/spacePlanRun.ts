import { create } from "zustand";

import { cleanupApply, cleanupApplyStatus } from "../api/cleanup";
import { appUnregister, fsPathPresent } from "../api/ps5";
import { transferAddr } from "../lib/addr";
import { libraryMoveDeps } from "../lib/libraryMoveDeps";
import { defaultMoveSubpath, resolveMoveDestination } from "../lib/moveTarget";
import { runOrder, volumeOfPath, type SpacePlan } from "../lib/spacePlan";
import { useActivityHistoryStore } from "./activityHistory";
import { audit } from "./auditLog";
import { libraryMove, libraryMoveKey, runLibraryMove, stopLibraryMove, useLibraryMoveStore } from "./libraryMove";
import { withConsolePrefix } from "./roster";
import { readWithRetries } from "../lib/pollRead";

/**
 * Runs a space plan ("Do it"): the leftovers in one clean-up run, then each move, then each
 * uninstall, one after another, through the same calls Clean up, Games and Ready to play use.
 * Kept outside the screen so leaving Clean up doesn't lose what happened.
 */

export type StepState = "waiting" | "running" | "done" | "failed" | "skipped" | "stopped";

export interface StepProgress {
  state: StepState;
  /** The console's own words, or a reason token (dest_exists, still_present, both_copies). */
  detail?: string;
  bytesDone?: number;
  bytesTotal?: number;
}

export interface SpaceRunState {
  host: string | null;
  running: boolean;
  stopRequested: boolean;
  /** Step id → its progress. */
  steps: Record<string, StepProgress>;
  /** Step ids in the order they run. */
  order: string[];
  finished: boolean;
}

const IDLE: SpaceRunState = {
  host: null,
  running: false,
  stopRequested: false,
  steps: {},
  order: [],
  finished: false,
};

export const useSpaceRunStore = create<SpaceRunState & { reset: () => void }>((set) => ({
  ...IDLE,
  reset: () => set((s) => (s.running ? s : { ...IDLE })),
}));

const put = (id: string, p: Partial<StepProgress>) =>
  useSpaceRunStore.setState((s) => ({ steps: { ...s.steps, [id]: { ...s.steps[id], ...p } } }));

export interface MoveOutcome {
  phase: "done" | "cancelled" | "copy-failed" | "delete-failed";
  error: string | null;
}

/** What a run needs from the app. Injected so it is testable without a console. */
export interface SpaceRunDeps {
  /** Removes clean-up items; resolves with what happened to each. */
  cleanup: (
    ids: number[],
    onProgress: (freed: number) => void,
  ) => Promise<{ done: number[]; skipped: { id: number; reason: string }[]; failed: { id: number; reason: string }[] }>;
  pathExists: (path: string) => Promise<boolean>;
  move: (from: string, to: string, label: string, onProgress: (done: number, total: number) => void) => Promise<MoveOutcome>;
  /** Asks the running move to stop (its copy, not a removal under way). */
  stopMove: (from: string) => void;
  uninstall: (titleId: string, label: string) => Promise<{ stillPresent: boolean }>;
}

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Where a move lands: the same folder under the other drive (`/data/homebrew/X` →
 *  `/mnt/ext1/homebrew/X`), as the Games Move proposes by default. */
export function moveDestination(from: string, dest: string, volumes: string[]): string {
  const src = volumeOfPath(from, volumes) ?? "";
  return resolveMoveDestination(dest, src ? defaultMoveSubpath(from, src) : "", from);
}

/** Runs the selected steps of `plan` in order. A failed step doesn't stop the rest (they free
 *  room on their own); Stop does, after the step that is running. */
export async function runSpacePlan(
  host: string,
  plan: SpacePlan,
  volumes: string[],
  deps: SpaceRunDeps,
): Promise<void> {
  if (useSpaceRunStore.getState().running) return;
  const steps = runOrder(plan);
  const leftovers = steps.filter((s) => s.candidate.kind === "leftover");
  const others = steps.filter((s) => s.candidate.kind !== "leftover");
  const order = [...leftovers.map((s) => s.candidate.id), ...others.map((s) => s.candidate.id)];
  useSpaceRunStore.setState({
    host,
    running: true,
    stopRequested: false,
    finished: false,
    order,
    steps: Object.fromEntries(order.map((id) => [id, { state: "waiting" as StepState }])),
  });
  const stopped = () => useSpaceRunStore.getState().stopRequested;
  try {
    if (leftovers.length > 0 && !stopped()) {
      for (const s of leftovers) put(s.candidate.id, { state: "running" });
      try {
        const byCleanupId = new Map(leftovers.map((s) => [s.candidate.cleanupId as number, s.candidate.id]));
        const res = await deps.cleanup([...byCleanupId.keys()], () => {});
        for (const id of res.done) {
          const sid = byCleanupId.get(id);
          if (sid) put(sid, { state: "done" });
        }
        for (const r of res.skipped) {
          const sid = byCleanupId.get(r.id);
          if (sid) put(sid, { state: "skipped", detail: r.reason });
        }
        for (const r of res.failed) {
          const sid = byCleanupId.get(r.id);
          if (sid) put(sid, { state: "failed", detail: r.reason });
        }
        // Anything the run didn't mention (stopped on the console) was not removed.
        for (const s of leftovers) {
          if (useSpaceRunStore.getState().steps[s.candidate.id]?.state === "running")
            put(s.candidate.id, { state: "skipped", detail: "stopped" });
        }
      } catch (e) {
        for (const s of leftovers) put(s.candidate.id, { state: "failed", detail: msg(e) });
      }
    }

    for (const s of others) {
      const c = s.candidate;
      if (stopped()) {
        put(c.id, { state: "stopped" });
        continue;
      }
      put(c.id, { state: "running" });
      if (c.kind === "move" && c.path && s.dest) {
        const to = moveDestination(c.path, s.dest, volumes);
        try {
          // The copy refuses to overwrite: say why before it starts rather than after.
          if (await deps.pathExists(to)) {
            put(c.id, { state: "skipped", detail: "dest_exists" });
            continue;
          }
        } catch {
          // Unknown: let the move itself decide.
        }
        try {
          const out = await deps.move(c.path, to, c.label, (done, total) =>
            put(c.id, { bytesDone: done, bytesTotal: total }),
          );
          if (out.phase === "done") put(c.id, { state: "done" });
          else if (out.phase === "cancelled") put(c.id, { state: "stopped" });
          else if (out.phase === "delete-failed") put(c.id, { state: "failed", detail: "both_copies" });
          else put(c.id, { state: "failed", detail: out.error ?? "" });
        } catch (e) {
          put(c.id, { state: "failed", detail: msg(e) });
        }
      } else if (c.kind === "uninstall" && c.titleId) {
        try {
          const res = await deps.uninstall(c.titleId, c.label);
          put(c.id, res.stillPresent ? { state: "failed", detail: "still_present" } : { state: "done" });
        } catch (e) {
          put(c.id, { state: "failed", detail: msg(e) });
        }
      } else {
        put(c.id, { state: "skipped" });
      }
    }
  } finally {
    useSpaceRunStore.setState({ running: false, finished: true });
  }
}

/** Stop after the step that is running; a move under way stops its copy (the original stays). */
export function stopSpacePlan(deps: Pick<SpaceRunDeps, "stopMove">, plan: SpacePlan | null): void {
  const st = useSpaceRunStore.getState();
  if (!st.running) return;
  useSpaceRunStore.setState({ stopRequested: true });
  for (const s of plan?.steps ?? []) {
    if (s.candidate.kind === "move" && s.candidate.path && st.steps[s.candidate.id]?.state === "running")
      deps.stopMove(s.candidate.path);
  }
}

// ── The real calls ───────────────────────────────────────────────────────────

const APPLY_POLL_MS = 1000;

/** The run's calls against one console: Clean up's apply, the Games Move, and the uninstall
 *  Ready to play uses. */
export function consoleSpaceRunDeps(host: string, scanId: number, payloadVersion: string | null): SpaceRunDeps {
  const addr = transferAddr(host);
  const history = () => useActivityHistoryStore.getState();
  return {
    async cleanup(ids, onProgress) {
      const { apply_id } = await cleanupApply(host, scanId, ids);
      const read = () => readWithRetries(() => cleanupApplyStatus(host, apply_id));
      let st = await read();
      while (st.state === "running") {
        onProgress(st.freed_bytes);
        await new Promise((r) => setTimeout(r, APPLY_POLL_MS));
        st = await read();
      }
      return { done: st.done, skipped: st.skipped, failed: st.failed };
    },
    pathExists: (path) => fsPathPresent(addr, path),
    async move(from, to, label, onProgress) {
      let activityId = "";
      await runLibraryMove({ host, addr, from, to, payloadVersion }, libraryMoveDeps, {
        onStart: (opId) => {
          activityId = history().start("library-move", `Moving ${label}`, { fromPath: from, toPath: to, opId, addr });
        },
        onProgress: (bytes, totalBytes) => {
          onProgress(bytes, totalBytes);
          history().update(activityId, { bytes, totalBytes });
        },
        onEnd: (m) => {
          if (m.phase === "done") history().finish(activityId, "done", { error: undefined });
          else if (m.phase === "cancelled") history().finish(activityId, "stopped", { error: "cancelled by user" });
          else history().finish(activityId, "failed", { error: m.error ?? undefined });
        },
      });
      const key = libraryMoveKey(host, from);
      const m = libraryMove(useLibraryMoveStore.getState(), key);
      if (!m) return { phase: "copy-failed", error: "move did not start" };
      // A move already running for this entry (started from Games) is left alone.
      if (m.phase === "copying" || m.phase === "deleting") return { phase: "copy-failed", error: "already moving" };
      return { phase: m.phase, error: m.error };
    },
    stopMove: (from) => stopLibraryMove(libraryMoveKey(host, from)),
    async uninstall(titleId, label) {
      const res = await appUnregister(addr, titleId);
      audit("app_uninstall", withConsolePrefix(host, label), { context: titleId });
      return res;
    },
  };
}
