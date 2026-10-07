// The auto-loader's same-boot guard.
//
// The auto-run playlist fires whenever a console's helper comes up. A helper comes up after a
// real boot, but also after a Wi-Fi blip, a helper update or the app reconnecting, and then the
// playlist's payloads are still running: sending kstuff or ShadowMount+ a second time stacks a
// second copy on the first (seen as 2 to 8 kstuff instances on one console). The console's boot
// time alone is not enough to tell: rest mode kills every payload without changing it, and then
// the playlist must run again.
//
// So a run is skipped only on positive evidence: it is the same boot, AND every payload the
// last auto-run left running is still running. Anything else (a reboot, a wake from rest mode,
// a console that cannot be asked) runs the playlist, as before.

import { safeGetItem, safeSetItem } from "./safeStorage";

/** Two readings of one boot differ by the time the requests took and by clock corrections. */
const SAME_BOOT_SLACK_MS = 120_000;

/** Names every loader gives to whatever it starts, or that are ours: they say nothing about
 *  which payloads are loaded (the installer daemon comes and goes as `payload.elf`). */
const GENERIC = new Set(["payload.elf", "elfldr.elf", "ps5upload.elf", "ps5upload-installer.elf"]);

export interface AutoRunRecord {
  /** When the console booted, on this computer's clock. */
  bootEpochMs: number;
  /** The distinctly named payload processes seen running after the auto-run. */
  payloads: string[];
}

/** When the console booted, from its uptime. */
export function bootEpochMs(nowMs: number, uptimeSec: number): number {
  return nowMs - uptimeSec * 1000;
}

/** The payload processes worth remembering: `.elf` processes that are not the console's own
 *  and not generically named. `system` lists what was running before any payload was sent,
 *  when known; console processes such as `SceSysCore.elf` are always there and prove nothing. */
export function distinctPayloads(processNames: readonly string[]): string[] {
  const seen = new Set<string>();
  for (const name of processNames) {
    if (!name.endsWith(".elf") || GENERIC.has(name)) continue;
    // The console's own daemons: Sce*.elf, and the handful without the prefix.
    if (/^(Sce|Agc|orbis_|mini-syscore)/.test(name)) continue;
    seen.add(name);
  }
  return [...seen].sort();
}

/** Whether the auto-run may be skipped: same boot, and every payload it left is still there. */
export function autoRunStillLoaded(
  record: AutoRunRecord | null,
  bootNowMs: number | null,
  runningNames: readonly string[] | null,
): boolean {
  if (!record || bootNowMs === null || runningNames === null) return false;
  if (record.payloads.length === 0) return false;
  if (Math.abs(record.bootEpochMs - bootNowMs) > SAME_BOOT_SLACK_MS) return false;
  const running = new Set(runningNames);
  return record.payloads.every((p) => running.has(p));
}

const key = (host: string) => `ps5upload.autoloader.ran.${host}`;

export function loadAutoRun(host: string): AutoRunRecord | null {
  try {
    const raw = safeGetItem(key(host));
    if (!raw) return null;
    const v = JSON.parse(raw) as Partial<AutoRunRecord>;
    if (typeof v.bootEpochMs !== "number" || !Array.isArray(v.payloads)) return null;
    return {
      bootEpochMs: v.bootEpochMs,
      payloads: v.payloads.filter((p): p is string => typeof p === "string"),
    };
  } catch {
    return null;
  }
}

export function rememberAutoRun(host: string, record: AutoRunRecord): void {
  safeSetItem(key(host), JSON.stringify(record));
}
