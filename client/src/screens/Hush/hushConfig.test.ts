import { describe, expect, it } from "vitest";

import type { ProcessInfo } from "../../api/ps5";
import {
  HUSH_DEFAULTS,
  hushLockPid,
  hushLogTail,
  hushPauseWaitMs,
  hushProcess,
  hushRevertDone,
  newLogLines,
  parseHushConfig,
  setHushEnabled,
} from "./hushConfig";

/** The config Hush 1.0.0 writes on its first run (write_default_config). */
const DEFAULT_INI = `# hush
# enabled=1 stops the recent-gameplay recording.
# enabled=0 restores the stock behaviour and exits.
enabled=1
# mode=check reports what would be patched and changes nothing
mode=apply
verbose=0
interval_s=20
rescan_s=300
`;

const proc = (o: Partial<ProcessInfo>): ProcessInfo => ({
  pid: 1,
  name: "",
  comm: "",
  title_id: "",
  app_id: 0,
  memory_mib: 1,
  threads: 1,
  kind: "payload",
  ...o,
});

describe("parseHushConfig", () => {
  it("reads Hush's own default config", () => {
    expect(parseHushConfig(DEFAULT_INI)).toEqual(HUSH_DEFAULTS);
  });

  it("follows Hush's rules: comments skipped, trimmed, last line wins, bad values ignored", () => {
    const c = parseHushConfig("# enabled=0\r\n enabled = 0 \r\nmode=check\r\ninterval_s=2\r\nverbose=1\r\n");
    expect(c).toMatchObject({ enabled: false, mode: "check", intervalS: 20, verbose: true });
    expect(parseHushConfig("enabled=0\nenabled=1\n").enabled).toBe(true);
    expect(parseHushConfig("enabled=yes\n").enabled).toBe(true);
    expect(parseHushConfig("enabled=0\nenabled=no\n").enabled).toBe(false);
    expect(parseHushConfig("mode=nonsense\ninterval_s=90\n")).toMatchObject({ mode: "apply", intervalS: 90 });
    expect(parseHushConfig("")).toEqual(HUSH_DEFAULTS);
  });
});

describe("setHushEnabled", () => {
  it("rewrites only the enabled line, keeping every other byte", () => {
    const off = setHushEnabled(DEFAULT_INI, false);
    expect(off).toBe(DEFAULT_INI.replace("\nenabled=1\n", "\nenabled=0\n"));
    expect(parseHushConfig(off).enabled).toBe(false);
    expect(setHushEnabled(off, true)).toBe(DEFAULT_INI);
  });

  it("keeps CRLF, spacing and comments that mention enabled", () => {
    const text = "# enabled=1 is on\r\n  enabled =  1\r\nmode=apply\r\n";
    expect(setHushEnabled(text, false)).toBe("# enabled=1 is on\r\n  enabled =  0\r\nmode=apply\r\n");
  });

  it("sets every enabled line, since Hush takes the last", () => {
    const out = setHushEnabled("enabled=1\nmode=apply\nenabled=1\n", false);
    expect(out).toBe("enabled=0\nmode=apply\nenabled=0\n");
  });

  it("adds the line when there is none", () => {
    expect(setHushEnabled("mode=apply\n", false)).toBe("mode=apply\nenabled=0\n");
    expect(setHushEnabled("mode=apply", false)).toBe("mode=apply\nenabled=0\n");
    expect(setHushEnabled("mode=apply\r\n", true)).toBe("mode=apply\r\nenabled=1\r\n");
    expect(setHushEnabled("", false)).toBe("enabled=0\n");
  });
});

describe("running Hush", () => {
  it("reads pid.lock", () => {
    expect(hushLockPid("142\n")).toBe(142);
    expect(hushLockPid("")).toBeNull();
    expect(hushLockPid(null)).toBeNull();
    expect(hushLockPid("1\n")).toBeNull();
    expect(hushLockPid("abc")).toBeNull();
  });

  it("finds it by the lock's pid (elfldr names it payload.elf) or by name", () => {
    const procs = [
      proc({ pid: 101, comm: "payload.elf", is_self: true }),
      proc({ pid: 142, comm: "payload.elf" }),
      proc({ pid: 60, comm: "SceAvCapture", kind: "system" }),
    ];
    expect(hushProcess(procs, 142)?.pid).toBe(142);
    // A stale lock naming a system process is not Hush.
    expect(hushProcess(procs, 60)).toBeNull();
    // Nor is our own helper.
    expect(hushProcess(procs, 101)).toBeNull();
    expect(hushProcess(procs, null)).toBeNull();
    expect(hushProcess([proc({ pid: 7, comm: "hush.elf" })], null)?.pid).toBe(7);
  });
});

describe("Hush's log", () => {
  const before = "2026-10-31 10:00:00 hush started; firmware=13.60 (0x13600007) enabled=1 mode=apply\n";

  it("tells the lines written since a point", () => {
    const after = `${before}2026-10-31 10:00:20 sceVencStartEncode restored in 4 slot(s)\n2026-10-31 10:00:20 disabled in config, exiting\n`;
    const lines = newLogLines(before, after);
    expect(lines).toHaveLength(2);
    expect(hushRevertDone(lines)).toBe(true);
    expect(hushRevertDone(newLogLines(before, before))).toBe(false);
    // The old "restored" line of an earlier pause does not count again.
    expect(hushRevertDone(newLogLines(after, `${after}2026-10-31 11:00:00 hush started\n`))).toBe(false);
  });

  it("reads the whole log when it was rotated meanwhile", () => {
    expect(newLogLines(before, "2026-10-31 10:00:20 disabled in config, exiting\n")).toEqual([
      "2026-10-31 10:00:20 disabled in config, exiting",
    ]);
    expect(newLogLines("", "a\n\nb\n")).toEqual(["a", "b"]);
  });

  it("does not take an incomplete revert for done", () => {
    expect(hushRevertDone(["revert incomplete (2 restored, 1 failed)", "revert not verified (rc=-2), retrying"])).toBe(false);
  });

  it("keeps the last lines", () => {
    expect(hushLogTail("a\nb\n\nc\r\nd\n", 3)).toEqual(["b", "c", "d"]);
    expect(hushLogTail("", 3)).toEqual([]);
  });

  it("waits Hush's interval plus a margin", () => {
    expect(hushPauseWaitMs({ intervalS: 20 })).toBe(25_000);
    expect(hushPauseWaitMs({ intervalS: 5 })).toBe(25_000);
    expect(hushPauseWaitMs({ intervalS: 45 })).toBe(50_000);
    expect(hushPauseWaitMs({ intervalS: 3600 })).toBe(60_000);
  });
});
