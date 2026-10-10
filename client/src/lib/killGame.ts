import { appKill, processKill, processList } from "../api/ps5";

/** What closing a game needs: Sony's app id, and a pid for the SIGKILL fallback when the
 *  caller has one. Running-app lists from app_list_running carry no pid; it is then looked
 *  up from the process list by app id, only when app-kill did not work. */
export interface KillTarget {
  appId: number;
  pid?: number;
}

/** The one way the app closes a running game: Sony's app-kill by app id, then a SIGKILL of
 *  its pid. app-kill can THROW (FW 12.20 rejects the app id), so the fallback runs on a
 *  throw as well as on ok=false; treating both the same is what makes the SIGKILL path
 *  reachable. */
export async function killGame(addr: string, game: KillTarget): Promise<boolean> {
  let killed = false;
  if (game.appId) {
    try {
      killed = (await appKill(addr, game.appId)).ok;
    } catch {
      /* Sony's app-kill failed or threw: fall through to SIGKILL. */
    }
  }
  if (killed) return true;
  let pid = game.pid ?? 0;
  if (!pid && game.appId) {
    try {
      const { processes } = await processList(addr);
      // Only a game process, never the helper itself.
      pid = processes.find((p) => p.kind === "app" && p.app_id === game.appId && !p.is_self)?.pid ?? 0;
    } catch {
      /* No process list: nothing to SIGKILL, reported by the caller. */
    }
  }
  if (pid) {
    try {
      killed = (await processKill(addr, pid)).ok;
    } catch {
      /* SIGKILL failed too: reported by the caller. */
    }
  }
  return killed;
}
