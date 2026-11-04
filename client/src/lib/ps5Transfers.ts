import { useUploadQueueStore } from "../state/uploadQueue";
import { useTransferStore } from "../state/transfer";
import { fsUploadForHost, useFsUploadStore } from "../state/fsUpload";
import {
  bulkOpForHost,
  downloadForHost,
  useFsBulkOpStore,
  useFsDownloadOpStore,
} from "../state/fsBulkOp";
import { hostOf } from "./addr";

/**
 * Coordination for the PS5's single-client transfer port and the
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
 * transfer port / payload. With consoles running in parallel, an
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
  return queueBusy || oneShotBusy || filesTransferBusy(host);
}

/**
 * True while the Files screen is moving data on `host` (any console when omitted): an upload, a
 * download, or a copy, move or delete. These run in their own stores, so a large drag into Files
 * used to read as an idle console: a helper could be sent over it, and the pollers kept going.
 */
export function filesTransferBusy(host?: string): boolean {
  const uploads = useFsUploadStore.getState();
  const downloads = useFsDownloadOpStore.getState();
  const bulk = useFsBulkOpStore.getState();
  const busy = (h: string) =>
    !!fsUploadForHost(uploads, h).active ||
    downloadForHost(downloads, h).active ||
    !!bulkOpForHost(bulk, h).op;
  if (host) return busy(host);
  return [uploads.byHost, downloads.byHost, bulk.byHost].some((m) => Object.keys(m).some(busy));
}

/**
 * Bytes moved so far on `host`'s running transfers, or null when none reports any. A change here
 * is proof the helper is alive. Queue rows and Files runs are where this lives now: the one-shot
 * upload's phase has had no caller but its retry since uploads moved to the queue.
 */
export function transferBytesMoved(host: string): number | null {
  const key = hostOf(host);
  let total = 0;
  for (const it of useUploadQueueStore.getState().items) {
    if (it.status === "running" && hostOf(it.addr) === key) total += it.bytesSent ?? 0;
  }
  total += fsUploadForHost(useFsUploadStore.getState(), host).active?.sent ?? 0;
  const download = downloadForHost(useFsDownloadOpStore.getState(), host);
  if (download.active) total += download.bytesReceived;
  const phase = useTransferStore.getState().phasesByHost[key];
  if (phase?.kind === "running") total += phase.bytesSent;
  return total > 0 ? total : null;
}
