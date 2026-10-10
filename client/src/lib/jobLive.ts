// What a running (or just finished) transfer can say beyond bytes and files, all optional
// fields on the engine's job snapshot, so a field the engine does not send yet shows nothing:
//
//   phase = "skipping"            7z/RAR resume: the decoder is discarding data the console
//                                 already has (skip_done_bytes of skip_total_bytes)
//   bottleneck                    what limited the transfer: AVA1's own words, "network",
//                                 "source", "console drive", "console workers", "console memory"
//                                 (Running when the engine reports it live, and always in the
//                                 finished job's commit_ack)
//   commit_ack.warning            a finished upload whose files the console did not confirm
//                                 saved in place (the engine stopped waiting): shown as a warning
//   settling                      files are still settling on the console after the job
//                                 finished (the engine sees `unswept` > 0): "Finishing on the console"
//   settle_files_left/_total      with `settling`: how many files the console still has to make
//                                 permanent, and the most it had (the "N of M" and the time left)
//   route / route_reason          a console-to-console copy (#433): "direct" between the consoles,
//                                 or "relay" through this computer and why (live while running, and
//                                 in the finished job's commit_ack)
//
// The contract is written down in protocol/ava1/CLIENT_CONTRACT.md.

export type BottleneckCause = "network" | "source" | "disk" | "workers" | "memory";

export interface JobLive {
  skipping: boolean;
  skipDoneBytes: number;
  skipTotalBytes: number;
  bottleneck: BottleneckCause | null;
  settling: boolean;
  /** While settling: files the console still has to make permanent, and the most it had. Absent
   *  when the engine does not send the counts (the line then shows no number). */
  settleLeft?: number;
  settleTotal?: number;
  /** A finished job that carries the engine's "console did not confirm saving" warning. */
  unsettled?: boolean;
  /** A console-to-console copy's route, and why it went through this computer. */
  route?: "direct" | "relay";
  routeReason?: string;
}

/** The snapshot fields this module reads (all optional). */
export interface JobLiveFields {
  phase?: string | null;
  skip_done_bytes?: number | null;
  skip_total_bytes?: number | null;
  bottleneck?: string | null;
  settling?: boolean | null;
  settle_files_left?: number | null;
  settle_files_total?: number | null;
  route?: string | null;
  route_reason?: string | null;
  commit_ack?: {
    bottleneck?: string | null;
    warning?: string | null;
    route?: string | null;
    route_reason?: string | null;
  } | null;
}

/** Maps the engine's bottleneck word (`ps5upload_ava1::progress::bottleneck_name`) to a
 *  cause; `none`, empty or an unknown word is no cause. */
export function bottleneckCause(
  word: string | null | undefined,
): BottleneckCause | null {
  switch ((word ?? "").trim().toLowerCase()) {
    case "network":
      return "network";
    case "source":
      return "source";
    case "console drive":
    case "disk":
      return "disk";
    case "console workers":
    case "workers":
      return "workers";
    case "console memory":
    case "memory":
      return "memory";
    default:
      return null;
  }
}

/** The live notes of one snapshot, or undefined when it carries none (the common case today),
 *  so a state record only grows a field when there is something to show. */
export function jobLiveFromSnapshot(
  snap: JobLiveFields | null | undefined,
): JobLive | undefined {
  if (!snap) return undefined;
  const skipping = snap.phase === "skipping";
  const skipDoneBytes = Math.max(0, Number(snap.skip_done_bytes) || 0);
  const skipTotalBytes = Math.max(0, Number(snap.skip_total_bytes) || 0);
  const bottleneck = bottleneckCause(snap.bottleneck ?? snap.commit_ack?.bottleneck);
  const settling = snap.settling === true;
  const warning = snap.commit_ack?.warning;
  const unsettled = typeof warning === "string" && warning.trim() !== "";
  const routeWord = snap.route ?? snap.commit_ack?.route;
  const route = routeWord === "direct" || routeWord === "relay" ? routeWord : undefined;
  const reasonWord = (snap.route_reason ?? snap.commit_ack?.route_reason ?? "").trim();
  const routeReason = route === "relay" && reasonWord ? reasonWord : undefined;
  if (!skipping && !bottleneck && !settling && !unsettled && !route) return undefined;
  const counted =
    settling && typeof snap.settle_files_left === "number" && Number.isFinite(snap.settle_files_left);
  const settleLeft = counted ? Math.max(0, snap.settle_files_left as number) : 0;
  const settleTotal = counted
    ? Math.max(settleLeft, Number(snap.settle_files_total) || 0)
    : 0;
  return {
    skipping,
    skipDoneBytes,
    skipTotalBytes,
    bottleneck,
    settling,
    ...(counted ? { settleLeft, settleTotal } : {}),
    ...(unsettled ? { unsettled } : {}),
    ...(route ? { route } : {}),
    ...(routeReason ? { routeReason } : {}),
  };
}
