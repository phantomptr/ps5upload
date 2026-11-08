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
    default:
      return translate(lang, "payload_refused_loader", undefined);
  }
}
