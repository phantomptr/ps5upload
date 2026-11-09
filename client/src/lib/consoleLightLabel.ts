import type { ConsoleLight } from "./consolePower";
import type { Translator } from "../state/lang";

/** The words for one console's status light (tooltip and screen-reader text). */
export function consoleLightLabel(light: ConsoleLight, tr: Translator): string {
  switch (light) {
    case "up":
      return tr("status_ps5_connected", undefined, "Connected");
    case "pairing":
      return tr("status_ps5_needs_pairing", undefined, "Needs pairing");
    case "rest":
      return tr("status_ps5_rest_mode", undefined, "In rest mode");
    case "unreachable":
      return tr("status_ps5_not_answering", undefined, "Not answering");
    case "down":
      return tr("status_ps5_disconnected", undefined, "Not connected");
    default:
      // Not answered yet this run (the app just opened, or the check is being confirmed).
      return tr("status_console_checking", undefined, "Checking…");
  }
}
