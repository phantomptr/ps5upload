// Hush (disco-trooper/hush, GPL-3): the files it keeps on the console and how it reads them.
// Pure, so the Hush card, Remote Play's pause and the tests agree.
//
// Facts from Hush 1.0.0's source (src/main.c):
// - config.ini is read line by line every interval_s (20 s): a line starting with '#' is a
//   comment, else `key=value` with both sides trimmed; a later line wins. enabled takes only
//   "0" or "1" (anything else keeps the previous value; the default is 1).
// - enabled=0: Hush restores every slot it changed, logs "sceVencStartEncode restored in N
//   slot(s)" then "disabled in config, exiting", and EXITS. Turning it back on means
//   enabled=1 and loading hush.elf again.
// - pid.lock holds the running daemon's pid (removed when it exits cleanly). Hush does not
//   rename itself, so the process list shows it as the loader names it (elfldr: payload.elf).

import type { ProcessInfo } from "../../api/ps5";

export const HUSH_DIR = "/data/hush";
export const HUSH_CONFIG_PATH = `${HUSH_DIR}/config.ini`;
export const HUSH_LOG_PATH = `${HUSH_DIR}/log.txt`;
export const HUSH_LOCK_PATH = `${HUSH_DIR}/pid.lock`;

/** Hush checks its config this often by default. */
export const HUSH_DEFAULT_INTERVAL_S = 20;

export interface HushConfig {
  enabled: boolean;
  /** "apply" (patches) or "check" (reports once and exits; a resident daemon ignores it). */
  mode: "apply" | "check";
  verbose: boolean;
  intervalS: number;
  rescanS: number;
}

export const HUSH_DEFAULTS: HushConfig = {
  enabled: true,
  mode: "apply",
  verbose: false,
  intervalS: HUSH_DEFAULT_INTERVAL_S,
  rescanS: 300,
};

interface Assignment {
  key: string;
  value: string;
}

function assignment(line: string): Assignment | null {
  if (line.startsWith("#")) return null;
  const eq = line.indexOf("=");
  if (eq < 0) return null;
  return { key: line.slice(0, eq).trim(), value: line.slice(eq + 1).trim() };
}

/** config.ini as Hush reads it. Missing or unparsable values keep Hush's defaults. */
export function parseHushConfig(text: string): HushConfig {
  const c = { ...HUSH_DEFAULTS };
  for (const line of text.split(/\r?\n/)) {
    const a = assignment(line);
    if (!a) continue;
    const n = /^[+-]?\d+/.test(a.value) ? parseInt(a.value, 10) : 0;
    switch (a.key) {
      case "enabled":
        if (a.value === "0" || a.value === "1") c.enabled = a.value === "1";
        break;
      case "verbose":
        c.verbose = n !== 0;
        break;
      case "interval_s":
        if (n >= 5 && n <= 3600) c.intervalS = n;
        break;
      case "rescan_s":
        if (n >= 30 && n <= 86400) c.rescanS = n;
        break;
      case "mode":
        if (a.value === "check" || a.value === "apply") c.mode = a.value;
        break;
    }
  }
  return c;
}

/**
 * config.ini with Hush switched on or off: every `enabled=` line gets the new value (Hush
 * takes the last one, so all of them must agree), and everything else (comments, other keys,
 * spacing, line endings) stays as it was. With no `enabled=` line, one is added at the end.
 */
export function setHushEnabled(text: string, on: boolean): string {
  const value = on ? "1" : "0";
  const newline = text.includes("\r\n") ? "\r\n" : "\n";
  let found = false;
  const lines = text.split("\n").map((raw) => {
    const cr = raw.endsWith("\r") ? "\r" : "";
    const line = cr ? raw.slice(0, -1) : raw;
    const a = assignment(line);
    if (!a || a.key !== "enabled") return raw;
    found = true;
    const eq = line.indexOf("=");
    const lead = line.slice(eq + 1).match(/^\s*/)?.[0] ?? "";
    return `${line.slice(0, eq + 1)}${lead}${value}${cr}`;
  });
  if (found) return lines.join("\n");
  if (text === "") return `enabled=${value}${newline}`;
  const sep = text.endsWith("\n") ? "" : newline;
  return `${text}${sep}enabled=${value}${newline}`;
}

/** The pid in pid.lock, or null. */
export function hushLockPid(text: string | null): number | null {
  const m = text?.trim().match(/^\d+/);
  const pid = m ? parseInt(m[0], 10) : NaN;
  return Number.isFinite(pid) && pid > 1 ? pid : null;
}

/**
 * Hush's process, if it is running: the payload whose pid pid.lock names (Hush itself trusts
 * the lock only for a live payload), or a process named hush(.elf), should a loader name it so.
 */
export function hushProcess(procs: readonly ProcessInfo[], lockPid: number | null): ProcessInfo | null {
  const byName = (p: ProcessInfo) =>
    [p.comm, p.name].some((n) => n.trim().toLowerCase().replace(/\.(elf|bin)$/, "") === "hush");
  return (
    procs.find((p) => !p.is_self && lockPid !== null && p.pid === lockPid && p.kind === "payload") ??
    procs.find((p) => !p.is_self && byName(p)) ??
    null
  );
}

/** The log's lines that came after `before` (all of it when the log was rotated meanwhile). */
export function newLogLines(before: string, after: string): string[] {
  const added = before && after.startsWith(before) ? after.slice(before.length) : after;
  return added.split(/\r?\n/).filter((l) => l.trim() !== "");
}

/** Hush's log says it gave the encoder back and exited. */
export function hushRevertDone(lines: readonly string[]): boolean {
  return lines.some((l) => /disabled in config, exiting|restored in \d+ slot/.test(l));
}

/** The last `n` lines of the log, newest last. */
export function hushLogTail(text: string, n = 8): string[] {
  const lines = text.split(/\r?\n/).filter((l) => l.trim() !== "");
  return lines.slice(Math.max(0, lines.length - n));
}

/** How long to wait for Hush to give the encoder back after enabled=0: its check interval
 *  plus a margin, 25 s by default, never more than a minute. */
export function hushPauseWaitMs(c: Pick<HushConfig, "intervalS">): number {
  return Math.min(60, Math.max(25, c.intervalS + 5)) * 1000;
}
