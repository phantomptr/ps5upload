// The console operations a dump swap (lib/dumpSwap.ts) runs on, bound to one PS5.

import {
  appUnregister,
  isPathMissingError,
  fsDelete,
  fsListDir,
  fsMkdir,
  fsMove,
  fsPathPresent,
  fsReadPreview,
  fsWriteText,
} from "../api/ps5";
import { pkgLibraryStore } from "../state/pkgLibrary";
import { mgmtAddr, transferAddr } from "./addr";
import type { SwapDeps } from "./dumpSwap";
import { fetchRunningGames } from "./runningGames";
import { decodeBase64Text } from "./base64Text";

export function consoleSwapDeps(host: string): SwapDeps {
  const tx = transferAddr(host);
  const mgmt = mgmtAddr(host);
  // Every "is it there?" below is a decision about moving or deleting a game, so a console that
  // could not answer rejects rather than reading as "not there".
  return {
    exists: (p) => fsPathPresent(tx, p),
    mkdir: async (p) => {
      if (!(await fsPathPresent(tx, p))) await fsMkdir(tx, p);
    },
    move: (a, b) => fsMove(tx, a, b),
    writeText: async (p, t) => {
      const r = await fsWriteText(mgmt, p, t);
      if (!r.ok) throw new Error(`write ${p}: ${r.err ?? "refused"}`);
    },
    readText: async (p) => {
      if (!(await fsPathPresent(tx, p))) return null;
      const r = await fsReadPreview(mgmt, p, 256 * 1024);
      return decodeBase64Text(r.base64);
    },
    remove: async (p) => {
      if (await fsPathPresent(tx, p)) await fsDelete(tx, p);
    },
    // A folder that is not there lists as empty; any other failure rejects.
    list: async (dir) => {
      try {
        return (await fsListDir(tx, dir)).map((e) => e.name);
      } catch (e) {
        if (isPathMissingError(e)) return [];
        throw e;
      }
    },
    isRunning: async (titleId) => (await fetchRunningGames(mgmt)).has(titleId),
    // A dump SMP or ps5upload registered has an app folder; appUnregister removes it (and
    // leaves the saves, measured on FW 5.10).
    isRegistered: (titleId) => fsPathPresent(tx, `/user/app/${titleId}`),
    // Installing over a title the PS5 still lists can wipe it; a swap undoes instead (#476).
    unregister: async (titleId) => {
      const { stillPresent } = await appUnregister(tx, titleId);
      if (stillPresent) throw new Error(`${titleId} is still on the console`);
    },
    // Through the console queue, like every install; Stream & install is the reliable route.
    install: (packagePath) => pkgLibraryStore(host).getState().installStream(packagePath, host),
    sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
    now: () => Date.now(),
  };
}
