import { useUploadQueueStore } from "../state/uploadQueue";
import { useTransferStore } from "../state/transfer";
import { hostOf } from "./addr";

/**
 * Coordination for the PS5's single-client transfer port (:9113) and the
 * payload itself. Only ONE of {Upload-screen one-shot, Upload-screen queue,
 * Install-screen .pkg upload, Install-screen install} can safely touch the
 * console at a time — an install swaps the payload out, and the transfer port
 * serves one client. When they overlap the loser dies with "PS5 stopped
 * responding". These helpers let a caller WAIT its turn instead of colliding.
 */

/**
 * True while an Upload-screen transfer (one-shot or queue) holds the port.
 *
 * Callers on the Install screen poll this (alongside their own .pkg-upload
 * state) to WAIT their turn instead of colliding — installing swaps the
 * payload, and the transfer port serves one client, so an install or .pkg
 * upload that starts mid-transfer kills the loser with "PS5 stopped
 * responding". No hard timeout is needed: a legitimate multi-GB upload can run
 * a long time and the transfers have their own stall watchdogs, so this always
 * clears eventually; cancellation is the escape hatch.
 *
 * **Pass `host` in multi-console contexts.** The collision being avoided is
 * per-PS5: it only happens when two operations target the SAME console's
 * :9113 transfer port / payload. With consoles running in parallel, an
 * unscoped check would make an install on console B wait — unbounded — behind
 * an unrelated upload on console A. When `host` is given, only work targeting
 * that console counts as busy. When omitted, falls back to the legacy global
 * OR (any host busy → busy) for the single-console call sites.
 */
export function transferScreenBusy(host?: string): boolean {
  const wantKey = host ? hostOf(host) : null;
  const uq = useUploadQueueStore.getState();
  const queueBusy = wantKey
    ? uq.items.some(
        (it) => it.status === "running" && hostOf(it.addr) === wantKey,
      )
    : uq.running;
  const ts = useTransferStore.getState();
  const active = (p?: { kind: string }) =>
    p?.kind === "starting" || p?.kind === "running";
  // One-shot transfers are now per-console (phasesByHost). Scoped: only this
  // host's phase counts. Unscoped (legacy global call sites): any console busy.
  const oneShotBusy = wantKey
    ? active(ts.phasesByHost[wantKey])
    : Object.values(ts.phasesByHost).some(active);
  return queueBusy || oneShotBusy;
}
