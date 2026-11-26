import { isRemotePath } from "./remotePath";
import { isStreamUnreachableError } from "./networkFix";

/** The bits of a queue item this needs (kept narrow so tests need no whole QueueItem). */
export interface StreamFailFacts {
  status: string;
  sourceKind?: string;
  install?: { via?: string; source?: string } | null;
  error?: string | null;
  /** The engine said the PS5 could not reach this computer (in whatever language `error` is). */
  streamUnreachable?: boolean;
}

/**
 * What a failed install row offers when it was a Stream & install of a file on this computer:
 * the PS5 fetches the package from this computer, so whatever the error, the two things worth
 * trying are letting the PS5 reach this computer (the firewall) and Upload & install, which
 * copies the package to the PS5 first and needs no connection back.
 *
 * - `retryViaUpload`: the row can be re-run as Upload & install (only a local file can).
 * - `hint`: say both. Not when the error already is the unreachable-stream guidance, which
 *   names the firewall itself and comes with its own network fix buttons.
 */
export function streamInstallFailHelp(item: StreamFailFacts): {
  retryViaUpload: boolean;
  hint: boolean;
} {
  const localStream =
    item.status === "failed" &&
    item.sourceKind === "install" &&
    item.install?.via === "stream" &&
    !!item.install.source &&
    !isRemotePath(item.install.source);
  return {
    retryViaUpload: localStream,
    hint: localStream && !item.streamUnreachable && !isStreamUnreachableError(item.error),
  };
}
