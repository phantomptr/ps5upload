import type { StreamPhase } from "./remotePlayStream";
import type { useTr } from "../state/lang";

type Tr = ReturnType<typeof useTr>;

/** What a stream that is not playing yet is doing, for the places that show its picture. */
export function phaseText(tr: Tr, p: StreamPhase): string {
  switch (p) {
    case "connecting":
      return tr("rp_phase_connecting", undefined, "Connecting…");
    case "waking":
      return tr("rp_phase_waking", undefined, "Waking the PS5 (about 20 s)…");
    case "starting":
      return tr("rp_phase_starting", undefined, "Starting the stream…");
    case "reconnecting":
      return tr("rp_phase_reconnecting", undefined, "Reconnecting…");
    default:
      return "";
  }
}
