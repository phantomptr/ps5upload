import { appKill, processKill } from "../api/ps5";
import type { RunningGame } from "./runningGames";

/** Closes a running game: Sony's app-kill by app id, then a SIGKILL of its pid. app-kill can
 *  THROW (FW 12.20 rejects the app id), so the fallback runs on a throw as well as on
 *  ok=false; treating both the same is what makes the SIGKILL path reachable. */
export async function killGame(addr: string, game: RunningGame): Promise<boolean> {
  let killed = false;
  if (game.appId) {
    try {
      killed = (await appKill(addr, game.appId)).ok;
    } catch {
      /* Sony's app-kill failed or threw: fall through to SIGKILL. */
    }
  }
  if (!killed && game.pid) {
    try {
      killed = (await processKill(addr, game.pid)).ok;
    } catch {
      /* SIGKILL failed too: reported by the caller. */
    }
  }
  return killed;
}
