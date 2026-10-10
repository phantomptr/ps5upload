import type { CheatsStatusResponse } from "../../api/ps5";
import type { ToastOptions } from "../../state/toasts";

type Tr = (key: string, vars?: Record<string, string | number>, fallback?: string) => string;

/** What to say after "Re-apply to running game", from the status read back
 *  afterwards: the game may have closed in the meantime, and then nothing was
 *  applied to anything. */
export function reapplyToast(status: CheatsStatusResponse | null, tr: Tr): ToastOptions {
  if (status?.game_running) {
    return {
      tone: "success",
      message: tr(
        "cheats_toast_reapplied",
        { game: status.game_title_id },
        `Cheats re-applied to ${status.game_title_id}.`,
      ),
    };
  }
  return {
    tone: "info",
    message: tr(
      "cheats_toast_reapply_no_game",
      undefined,
      "No game is running, so nothing was re-applied. The cheat files were read again.",
    ),
  };
}
