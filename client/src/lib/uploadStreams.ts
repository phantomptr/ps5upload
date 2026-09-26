import { useUploadSettingsStore } from "../state/uploadSettings";
import { useConnectionStore, EMPTY_HOST_RUNTIME } from "../state/connection";
import { hostOf } from "./addr";

/**
 * Resolve the parallel upload stream count to actually use for a transfer:
 * the user's setting, clamped by what THAT console's payload advertises
 * (`max_transfer_streams`). A payload that predates multi-stream advertises
 * nothing → treated as 1, so multi-stream silently no-ops on old payloads and
 * the engine takes its single-stream path. Returns at least 1, so callers can
 * always pass the result straight through to `startTransferDirReconcile`.
 *
 * `addr` is the target console (bare host or `ip:port` — port is stripped).
 * It must be the console the transfer actually targets, NOT the active tab:
 * the queue drains every console in parallel, and reading the active tab's
 * advertised capability for a background console's transfer hands console B
 * console A's stream count (multi-console audit, 2.31.0). Falls back to the
 * active console's mirror only when no addr is given (legacy callers).
 *
 * Lives in its own module (rather than in a store) so neither the upload-
 * settings nor connection store has to import the other, avoiding a cycle.
 */
export function effectiveUploadStreams(addr?: string): number {
  const want = useUploadSettingsStore.getState().uploadStreams;
  const conn = useConnectionStore.getState();
  const payloadMax = addr
    ? ((conn.runtimeByHost[hostOf(addr) || "_"] ?? EMPTY_HOST_RUNTIME)
        .maxTransferStreams ?? 1)
    : (conn.maxTransferStreams ?? 1);
  return Math.max(1, Math.min(want, payloadMax));
}
