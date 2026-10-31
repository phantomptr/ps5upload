import { describe, expect, it } from "vitest";

import type { PayloadInfo, ProcessInfo } from "../../../api/ps5";
import { detectHubCards, entryFromCatalog, hubEntries, markerFolders, normProcName } from "./registry";

const proc = (pid: number, comm: string, kind: ProcessInfo["kind"] = "payload", extra: Partial<ProcessInfo> = {}): ProcessInfo => ({
  pid,
  name: comm,
  comm,
  title_id: "",
  app_id: 0,
  memory_mib: 4,
  threads: 3,
  kind,
  ...extra,
});

const info = (id: string, display: string, hint: string | null, marker: string | null, ports: number[] = []): PayloadInfo => ({
  id,
  display_name: display,
  role: "",
  description: "",
  repo_owner: "o",
  repo_name: "r",
  on_console_marker_path: marker,
  process_name_hint: hint,
  ports,
  autoload_priority: 0,
  autoload_delay_ms: 0,
  homepage: "",
});

// A slice of the desktop catalogue (commands/payloads.rs).
const CATALOG: PayloadInfo[] = [
  info("kstuff-echostretch", "kstuff", null, "/data/kstuff.elf"),
  info("kstuff-lite-drakmor", "kstuff-lite", null, "/data/kstuff.elf"),
  info("shadowmountplus", "ShadowMount+", "shadowmountplus", "/data/shadowmount/debug.log"),
  info("ftpsrv", "ftpsrv", "ftpsrv", null, [2121]),
  info("payload-manager", "Payload Manager", null, "/data/pldmgr/pldmgr_config.txt", [8084]),
  info("nanodns", "nanoDNS", "nanodns.elf", "/data/nanodns/nanodns.ini", [53]),
  info("hearbridge", "HearBridge", "hearbridge.elf", "/data/hearbridge/hearbridge.log", [8090]),
  info("klogsrv", "klogsrv", "klogsrv.elf", "/data/klog/klog.log", [3232]),
];

// The FW 13.60 Phat's process list (payloads + the helper), 2026-10-18.
const PHAT: ProcessInfo[] = [
  proc(130, "ps5upload-tile", "system", { is_self: true }),
  proc(129, "shadowmountplus.elf"),
  proc(128, "kstuff.elf"),
  proc(127, "elfldr.elf"),
  proc(107, "kstuff.elf"),
  proc(97, "ftpsrv.elf"),
  proc(96, "nanodns.elf"),
  proc(90, "pldmgr.elf"),
  proc(60, "SceShellUI", "system"),
];

describe("On your PS5 detection", () => {
  it("finds every catalogued payload running on the Phat, integrations first", () => {
    const cards = detectHubCards({ entries: hubEntries(CATALOG), processes: PHAT, markers: new Set() });
    expect(cards.map((c) => [c.id, c.state])).toEqual([
      ["nanodns", "running"],
      ["payload-manager", "running"],
      ["shadowmountplus", "running"],
      ["proc:elfldr", "running"],
      ["ftpsrv", "running"],
      ["kstuff-echostretch", "running"],
    ]);
    const kstuff = cards.find((c) => c.id === "kstuff-echostretch")!;
    // Two catalogue builds, one process name: one card naming both, both pids.
    expect(kstuff.entryIds).toEqual(["kstuff-echostretch", "kstuff-lite-drakmor"]);
    expect(kstuff.name).toBe("kstuff / kstuff-lite");
    expect(kstuff.processes.map((p) => p.pid)).toEqual([128, 107]);
    expect(cards.find((c) => c.id === "payload-manager")!.webPort).toBe(8084);
    expect(cards.find((c) => c.id === "proc:elfldr")!.name).toBe("elfldr.elf");
  });

  it("shows HyperHEN's built-in ShadowMount+ engine on the ShadowMount+ card", () => {
    const procs = [proc(140, "hyperhen_mount.elf"), proc(141, "HyperHEN.elf")];
    for (const entries of [hubEntries(CATALOG), hubEntries(null)]) {
      const cards = detectHubCards({ entries, processes: procs, markers: new Set() });
      const smp = cards.find((c) => c.id === "shadowmountplus")!;
      expect(smp.state).toBe("running");
      expect(smp.processes.map((p) => p.pid)).toEqual([140]);
    }
  });

  it("never shows the helper itself or system processes", () => {
    const cards = detectHubCards({ entries: hubEntries(CATALOG), processes: PHAT, markers: new Set() });
    expect(cards.some((c) => c.processes.some((p) => p.is_self || p.kind === "system"))).toBe(false);
  });

  it("shows a payload whose marker is on the console as installed", () => {
    const cards = detectHubCards({
      entries: hubEntries(CATALOG),
      processes: [],
      markers: new Set(["/data/hearbridge/hearbridge.log", "/data/klog/klog.log"]),
    });
    expect(cards.map((c) => [c.id, c.state])).toEqual([
      ["hearbridge", "installed"],
      ["klogsrv", "installed"],
    ]);
  });

  it("leaves out what is neither running nor installed, unless a link opened it", () => {
    const entries = hubEntries(CATALOG);
    expect(detectHubCards({ entries, processes: [], markers: new Set() })).toEqual([]);
    const forced = detectHubCards({ entries, processes: [], markers: new Set(), forceShow: "hearbridge" });
    expect(forced.map((c) => [c.id, c.state])).toEqual([["hearbridge", "unknown"]]);
  });

  it("counts an integration whose own API answered as running", () => {
    const cards = detectHubCards({
      entries: hubEntries(CATALOG),
      processes: [],
      markers: new Set(["/data/hearbridge/hearbridge.log"]),
      answering: new Set(["hearbridge"]),
    });
    expect(cards.map((c) => [c.id, c.state, c.integration, c.webPort])).toEqual([
      ["hearbridge", "running", "hearbridge", 8090],
    ]);
  });

  it("finds Hush by the pid in its lock file: elfldr shows it as payload.elf", () => {
    const procs = [proc(101, "payload.elf", "payload", { is_self: true }), proc(142, "payload.elf"), proc(127, "elfldr.elf")];
    for (const entries of [hubEntries([...CATALOG, info("hush", "Hush", "hush.elf", "/data/hush/config.ini")]), hubEntries(null)]) {
      const cards = detectHubCards({
        entries,
        processes: procs,
        markers: new Set(["/data/hush/config.ini"]),
        pids: new Map([["hush", [142]]]),
      });
      const hush = cards.find((c) => c.id === "hush")!;
      expect([hush.state, hush.integration]).toEqual(["running", "hush"]);
      expect(hush.processes.map((p) => p.pid)).toEqual([142]);
      // Its process is not shown again as an uncatalogued payload.elf.
      expect(cards.some((c) => c.id === "proc:payload")).toBe(false);
    }
    // Without the lock (Hush exited), its config marks it installed.
    const off = detectHubCards({ entries: hubEntries(null), processes: [], markers: new Set(["/data/hush/config.ini"]) });
    expect(off.map((c) => [c.id, c.state])).toEqual([["hush", "installed"]]);
  });

  it("works without a catalogue (the browser build) for the integrations", () => {
    const cards = detectHubCards({ entries: hubEntries(null), processes: PHAT, markers: new Set() });
    expect(cards.map((c) => c.id)).toEqual([
      "nanodns",
      "payload-manager",
      "shadowmountplus",
      "proc:elfldr",
      "proc:ftpsrv",
      "proc:kstuff",
    ]);
    expect(cards.find((c) => c.id === "proc:kstuff")!.processes).toHaveLength(2);
  });

  it("compares process names without case or extension", () => {
    expect(normProcName("HearBridge.ELF")).toBe("hearbridge");
    expect(normProcName("etaHEN")).toBe("etahen");
    expect(entryFromCatalog(CATALOG[4]).processNames).toEqual(["pldmgr"]);
    expect(entryFromCatalog(CATALOG[5]).webPort).toBeNull();
  });

  it("lists each marker folder once", () => {
    const folders = markerFolders(hubEntries(CATALOG));
    expect(folders.get("/data")).toEqual(["/data/kstuff.elf"]);
    expect(folders.get("/data/pldmgr")).toEqual(["/data/pldmgr/pldmgr_config.txt"]);
  });
});
