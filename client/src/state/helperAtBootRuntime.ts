// lib/helperAtBoot wired to the real console, engine and app (#715).

import { pmOverview, pmSetAutoload } from "../api/payloadManager";
import {
  bundledPayloadPath,
  fetchHwPower,
  helperReplaceCopy,
  helperRestorePrevious,
  helperStoreInPldmgr,
} from "../api/ps5";
import { hostOf } from "../lib/addr";
import { getAppVersion } from "../lib/appVersion";
import { bootEpochMs } from "../lib/autoLoaderGuard";
import {
  disableHelperAtBoot,
  enableHelperAtBoot,
  loadBootRecord,
  saveBootRecord,
  syncHelperAtBoot,
  type SyncResult,
} from "../lib/helperAtBoot";
import { sentAgoMs } from "../lib/helperSendGate";
import { isTauriEnv } from "../lib/tauriEnv";
import { trStatic } from "../lib/trStatic";
import { useConnectionStore } from "./connection";
import { log } from "./logs";
import { pushNotification } from "./notifications";
import { withConsolePrefix } from "./roster";

/** The desktop app stores the helper it carries; the browser build has none, and the engine
 *  uses its own. */
const localHelper = (): Promise<string | null> =>
  isTauriEnv() ? bundledPayloadPath().catch(() => null) : Promise.resolve(null);

/** What was read, written and read back, for a bug report (#781). */
const bootLog = (line: string) => log.info("helper-boot", line);

const common = {
  pmOverview,
  setAutoload: pmSetAutoload,
  log: bootLog,
  appVersion: () => getAppVersion(),
  now: () => Date.now(),
  load: loadBootRecord,
  save: saveBootRecord,
};

/** Turn "Load the helper at boot" on. `confirmReplace` is asked before storing into an existing
 *  `ps5upload` folder in Payload Manager (it removes that folder's other files);
 *  `confirmTurnOnAutoload` when Payload Manager's boot list is turned off. */
export async function turnOnHelperAtBoot(
  host: string,
  confirmReplace: (folder: string) => Promise<boolean>,
  confirmTurnOnAutoload?: () => Promise<boolean>,
): Promise<boolean> {
  const h = hostOf(host);
  const local = await localHelper();
  return enableHelperAtBoot(h, {
    ...common,
    store: async (target) => {
      let r = await helperStoreInPldmgr(target, local, false);
      if (!r.stored) {
        if (!(await confirmReplace(r.folder))) return false;
        r = await helperStoreInPldmgr(target, local, true);
      }
      return r.stored;
    },
    replace: (target, path) => helperReplaceCopy(target, path, local, true),
    confirmTurnOnAutoload,
  });
}

export async function turnOffHelperAtBoot(host: string): Promise<void> {
  await disableHelperAtBoot(hostOf(host), common);
}

const running = new Set<string>();

/** Once per helper connection (AppShell): judge or refresh the copy Payload Manager starts at
 *  boot, and say what happened when it matters. Never throws. */
export async function syncHelperAtBootNow(host: string): Promise<SyncResult> {
  const h = hostOf(host);
  if (!h || running.has(h) || !loadBootRecord(h)) return { kind: "off" };
  running.add(h);
  try {
    const local = await localHelper();
    const r = await syncHelperAtBoot(h, {
      ...common,
      replace: (target, path) => helperReplaceCopy(target, path, local, true),
      restore: helperRestorePrevious,
      bootEpochMs: async (target) => {
        const p = await fetchHwPower(target);
        return p.operating_time_sec > 0 ? bootEpochMs(Date.now(), p.operating_time_sec) : null;
      },
      appSentAt: (target) => {
        const ago = sentAgoMs(target);
        return ago === null ? null : Date.now() - ago;
      },
      runningVersion: (target) =>
        useConnectionStore.getState().runtimeByHost[hostOf(target)]?.payloadVersion ?? null,
    });
    report(h, r);
    return r;
  } catch (e) {
    return { kind: "error", error: e instanceof Error ? e.message : String(e) };
  } finally {
    running.delete(h);
  }
}

function report(host: string, r: SyncResult): void {
  switch (r.kind) {
    case "updated":
      log.info("helper-boot", `${host}: Payload Manager's helper copy updated to ${r.version}; the previous one is kept`);
      return;
    case "came-up":
      log.info("helper-boot", `${host}: the helper Payload Manager started at boot (${r.version}) came up`);
      return;
    case "restored":
      log.warn("helper-boot", `${host}: helper ${r.failed} did not come up at boot; put back ${r.back || "the previous copy"}`);
      pushNotification(
        "warning",
        withConsolePrefix(
          host,
          trStatic("helper_boot_restored_title", "The helper did not start at boot"),
        ),
        {
          body: trStatic(
            "helper_boot_restored_body",
            "The PS5 restarted but helper {version} that Payload Manager keeps did not come up, so this app sent one. The previous copy is back in Payload Manager and starts at the next restart. This app will not store {version} there again.",
            { version: r.failed },
          ),
        },
      );
      return;
    case "missing":
      log.warn("helper-boot", `${host}: the helper copy was deleted in Payload Manager; it no longer loads at boot`);
      return;
    case "restore-failed":
      log.warn("helper-boot", `${host}: helper ${r.failed} did not come up at boot; no previous copy put back (${r.error})`);
      return;
    case "error":
      log.warn("helper-boot", `${host}: could not check the helper Payload Manager starts at boot: ${r.error}`);
      return;
    default:
      return;
  }
}
