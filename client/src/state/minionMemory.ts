// Which consoles this computer turned PS5Upload Minion off (or removed it) on. The console's own
// settings file is the truth; this only matters when that file is gone (its /data/ps5upload folder
// was wiped, or the helper never saved one): the helper then starts with the defaults, Minion on,
// and the next connection from here turns it off again instead of letting it come back.

import { minionSet } from "../api/consoleFiles";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { log } from "./logs";
import { NOOP_RELOAD, registerSettingReload } from "../lib/liveSettings";

const KEY = "ps5upload.minionOff";

function read(): string[] {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "[]") as unknown;
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** Remember the choice made for `host` from this computer. */
export function rememberMinion(host: string, off: boolean): void {
  const h = hostOf(host);
  if (!h) return;
  const rest = read().filter((x) => x !== h);
  safeSetItem(KEY, JSON.stringify(off ? [...rest, h] : rest));
}

export function minionRememberedOff(host: string): boolean {
  return read().includes(hostOf(host));
}

/** What to do when the helper answers: turn Minion off again only when this computer turned it
 *  off, the console says it is on, and it has no settings file (so nobody chose "on" there). */
export function minionNeedsReapply(
  rememberedOff: boolean,
  state: { supported: boolean; enabled?: boolean; saved?: boolean },
): boolean {
  return rememberedOff && state.supported && state.enabled === true && state.saved === false;
}

// "Keep Minion off on consoles I add" (#715): a console this computer has not met yet gets Minion
// turned off once, when its helper first answers. Once: a console is remembered as handled, so
// someone who turns Minion back on for it keeps it on. Changing the switch on a console by hand
// counts as handled too.
const OFF_FOR_NEW_KEY = "ps5upload.minion_off_for_new";
const HANDLED_KEY = "ps5upload.minionDefaultDone";

function readHandled(): string[] {
  try {
    const v = JSON.parse(safeGetItem(HANDLED_KEY) ?? "[]") as unknown;
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function minionOffForNew(): boolean {
  return safeGetItem(OFF_FOR_NEW_KEY) === "true";
}

/** Turning it on leaves the consoles already known (`known`) as they are: it is for consoles
 *  added from now on. */
export function setMinionOffForNew(on: boolean, known: readonly string[] = []): void {
  if (on) for (const h of known) markMinionHandled(h);
  safeSetItem(OFF_FOR_NEW_KEY, on ? "true" : "false");
}

/** This console has had the default applied, or someone chose for it by hand. */
export function markMinionHandled(host: string): void {
  const h = hostOf(host);
  if (!h) return;
  const all = readHandled();
  if (!all.includes(h)) safeSetItem(HANDLED_KEY, JSON.stringify([...all, h]));
}

export function minionHandled(host: string): boolean {
  return readHandled().includes(hostOf(host));
}

/** Whether a console's first answer should turn Minion off: the setting is on, the console is
 *  new here, and its helper has the switch and says Minion is on. */
export function minionDefaultApplies(
  offForNew: boolean,
  handled: boolean,
  state: { supported: boolean; enabled?: boolean },
): boolean {
  return offForNew && !handled && state.supported && state.enabled === true;
}

async function applyNewConsoleDefault(host: string): Promise<void> {
  if (!minionOffForNew() || minionHandled(host)) return;
  try {
    const s = await minionSet(host);
    // An older helper without the switch: tried again once it is updated.
    if (!s.supported || typeof s.enabled !== "boolean") return;
    if (minionDefaultApplies(true, false, s)) {
      await minionSet(host, { enabled: false });
      rememberMinion(host, true);
      log.info("minion", `${hostOf(host)} is a new console here; turned Minion off as set in Settings`);
    }
    markMinionHandled(host);
  } catch (e) {
    log.warn("minion", `could not apply the Minion default on ${hostOf(host)}: ${e instanceof Error ? e.message : String(e)}`);
  }
}

/** Run once per helper connection (AppShell). */
export async function keepMinionOff(host: string): Promise<void> {
  if (!minionRememberedOff(host)) {
    await applyNewConsoleDefault(host);
    return;
  }
  try {
    const s = await minionSet(host);
    if (!s.supported || !minionNeedsReapply(true, s)) return;
    await minionSet(host, { enabled: false });
    log.info("minion", `${hostOf(host)} lost its Minion settings; turned Minion off again as chosen here`);
  } catch (e) {
    log.warn("minion", `could not check Minion on ${hostOf(host)}: ${e instanceof Error ? e.message : String(e)}`);
  }
}

// Read fresh at each use: nothing to reload (lib/liveSettings).
registerSettingReload([KEY, OFF_FOR_NEW_KEY, HANDLED_KEY], NOOP_RELOAD);
