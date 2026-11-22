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

// ── Per step: never send again what is already running ─────────────────────────────────────
//
// The check above is all or nothing: when the last auto-run left no distinctly named payload
// (kstuff started by a loader that names everything `payload.elf`), it never skips, and the
// whole playlist went again on every reconnect, kstuff included. Loading a kernel patcher twice
// can panic the console. So an automatic run also decides each step:
//
// - a step whose payload is in the process list is not sent;
// - a kernel patcher (kstuff and the like) that a playlist already sent since the console
//   started is not sent again, even when the process list cannot show it (generic name, no
//   helper to ask). Same boot: sent after the console started (its uptime), or, with no uptime,
//   the console was watched the whole time since the send and never went away.
//
// A playlist someone starts with Run sends everything, as before.

/** Payloads that patch the kernel: a second copy on a running one can panic the console. */
export const KERNEL_PATCHERS = ["kstuff", "etahen", "hyperhen", "byepervisor"] as const;

/** Names that say nothing about which payload runs (loaders' generic names, ours). */
const GENERIC_NAMES = new Set(["payload", "a.out", "elfldr", "ps5upload", "ps5upload-installer"]);

/** "KStuff.elf" → "kstuff": lower case, no .elf/.bin. */
export function normName(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/\.(elf|bin)$/, "");
}

/** What a step is known by: its file's name, and a repo step's id and label. */
export interface StepLike {
  path: string;
  payloadId?: string;
  payloadLabel?: string;
}

export function stepNames(step: StepLike): string[] {
  const file = (step.path || "").split(/[\\/]/).pop() ?? "";
  return [file, step.payloadId ?? "", step.payloadLabel ?? ""]
    .map(normName)
    .filter((n) => n.length > 0);
}

/** The kernel patcher a step sends ("kstuff"), or null. */
export function kernelPatcherOf(step: StepLike): string | null {
  const names = stepNames(step);
  return KERNEL_PATCHERS.find((k) => names.some((n) => n.includes(k))) ?? null;
}

/** Whether the process list shows this step's payload running. Generic names never count. */
export function stepRunning(step: StepLike, running: readonly string[]): boolean {
  const names = stepNames(step);
  const kernel = kernelPatcherOf(step);
  for (const raw of running) {
    const p = normName(raw);
    if (!p || GENERIC_NAMES.has(p) || /^(sce|agc|orbis_|mini-syscore)/.test(p)) continue;
    if (kernel && p.includes(kernel)) return true;
    for (const n of names) {
      if (n === p) return true;
      // "shadowmountplus-1.7" is the file of the process "shadowmountplus".
      if (p.length >= 4 && n.startsWith(p) && /^[-_. ]/.test(n.slice(p.length))) return true;
    }
  }
  return false;
}

/** What an automatic run knows about the console when it starts. */
export interface AutoRunContext {
  /** Process names, or null when they cannot be read (no helper). */
  running: readonly string[] | null;
  /** When the console started, on this computer's clock, or null when not known. */
  bootMs: number | null;
  /** Whether the console was watched without going away since `t` (ms): true, false (it went
   *  away since), or null (not watched then). */
  stayedSince: (t: number) => boolean | null;
}

export type AutoStepDecision =
  | { send: true }
  | { send: false; why: "running" | "sent-this-boot" };

/** Whether an automatic run sends this step. `sent`: kernel patcher → when a playlist last sent
 *  it to this console (ms, this computer's clock). */
export function autoStepDecision(
  step: StepLike,
  ctx: AutoRunContext,
  sent: Readonly<Record<string, number>>,
): AutoStepDecision {
  if (ctx.running && stepRunning(step, ctx.running)) return { send: false, why: "running" };
  const kernel = kernelPatcherOf(step);
  const at = kernel ? sent[kernel] : undefined;
  if (typeof at === "number") {
    // Sent after the console started: this boot. No uptime: only a console watched the whole
    // time since the send is known to be the same boot.
    const sameBoot = ctx.bootMs !== null ? at > ctx.bootMs : ctx.stayedSince(at) === true;
    if (sameBoot) return { send: false, why: "sent-this-boot" };
  }
  return { send: true };
}

const sentKey = (host: string) => `ps5upload.autoloader.kernelSent.${host}`;

/** When a playlist last sent each kernel patcher to `host` (ms), from this computer. */
export function loadKernelSends(host: string): Record<string, number> {
  try {
    const v = JSON.parse(safeGetItem(sentKey(host)) ?? "{}") as unknown;
    if (!v || typeof v !== "object" || Array.isArray(v)) return {};
    const out: Record<string, number> = {};
    for (const [k, t] of Object.entries(v as Record<string, unknown>)) {
      if (typeof t === "number") out[k] = t;
    }
    return out;
  } catch {
    return {};
  }
}

export function rememberKernelSend(host: string, patcher: string, at: number): void {
  safeSetItem(sentKey(host), JSON.stringify({ ...loadKernelSends(host), [patcher]: at }));
}
