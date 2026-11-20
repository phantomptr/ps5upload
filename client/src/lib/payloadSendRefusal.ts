// What a refused payload send says, in the user's language, wherever the send was started.

import { PayloadSendRefused } from "../api/ps5";
import { t as translate } from "../i18n";
import { useLangStore } from "../state/lang";
import { humanizeCaught } from "./humanizeError";

/** The sentence for a refused send; any other error as it is. */
export function payloadSendRefusalText(e: unknown): string {
  if (!(e instanceof PayloadSendRefused)) return humanizeCaught(e);
  const lang = useLangStore.getState().lang;
  switch (e.code) {
    case "pkg_install_busy":
      return translate(lang, "payload_refused_install", undefined);
    case "helper_busy":
      return translate(lang, "payload_refused_helper", undefined);
    // Reached through Tailscale now: a loader port is not carried there.
    case "tailnet_away":
      return translate(lang, "payload_refused_tailnet", undefined);
    // The engine's helper gate (12.6.15).
    case "recent_send":
      return translate(lang, "payload_refused_recent", undefined);
    case "console_busy":
      return translate(lang, "payload_refused_console_busy", undefined);
    case "send_budget_spent":
      return translate(lang, "payload_refused_budget", undefined);
    case "console_restarted_after_sends":
      return translate(lang, "reconnect_why_restarted", undefined);
    case "different_console":
      return translate(lang, "reconnect_why_different_console", undefined);
    case "helper_version_mismatch":
      return translate(lang, "reconnect_why_stale_build", undefined);
    case "engine_unreachable":
      return translate(lang, "payload_refused_engine", undefined);
    case "loader_busy":
      return translate(lang, "payload_refused_loader", undefined);
    default:
      // A code this app does not know (a newer engine): its own sentence, if it gave one.
      return e.detail ?? translate(lang, "payload_refused_loader", undefined);
  }
}
