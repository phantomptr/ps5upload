import { useCallback } from "react";

import { useConfirm } from "../components/ConfirmDialog";
import { useTr } from "../state/lang";
import { pushNotification } from "../state/notifications";
import { withConsolePrefix } from "../state/roster";
import { mgmtAddr } from "./addr";
import { killGame } from "./killGame";
import { closeRunningGameFirst } from "./launchSwap";
import { fetchRunningGames } from "./runningGames";

/** Every Play button's first step: if another game is running, ask, close it, and wait for
 *  the console to settle (see lib/launchSwap for why a launch over a running game fails).
 *
 *  `makeWay` resolves true when the launch may go ahead, false when the user kept the running
 *  game or it would not close (that case is reported here). Render `dialog` once. */
export function useMakeWay() {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const makeWay = useCallback(
    async (
      host: string,
      titleId: string,
      titleName: string,
      /** A name for another title's id, when the screen knows it. */
      nameOf: (id: string) => string = (id) => id,
    ): Promise<boolean> => {
      const addr = mgmtAddr(host);
      let otherName = "";
      const way = await closeRunningGameFirst(titleId, {
        running: () => fetchRunningGames(addr),
        confirm: (other) => {
          otherName = nameOf(other.titleId);
          return confirm({
            title: tr("installed_swap_confirm_title", { other: otherName }, "Close {other} first?"),
            message: tr(
              "installed_swap_confirm_body",
              { other: otherName, name: titleName },
              "{other} is running, and the PS5 runs one game at a time. Close it and start {name}? Any unsaved progress in {other} will be lost.",
            ),
            confirmLabel: tr("installed_swap_confirm_ok", undefined, "Close and start"),
            destructive: true,
          });
        },
        close: (other) => killGame(addr, other),
        sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
      });
      if (way === "close_failed") {
        pushNotification("error", withConsolePrefix(host, titleName), {
          body: tr(
            "installed_swap_close_failed",
            { other: otherName, name: titleName },
            "{other} is still running and could not be closed, so {name} was not started. Close it on the PS5 and press Play again.",
          ),
        });
      }
      return way === "clear" || way === "closed";
    },
    [confirm, tr],
  );
  return { makeWay, dialog };
}
