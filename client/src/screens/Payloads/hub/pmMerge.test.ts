import { describe, expect, it } from "vitest";

import type { PayloadInfo, ProcessInfo } from "../../../api/ps5";
import { groupCards, isOurProcess, matchSavedToCards, originOf, savedKey } from "./pmMerge";
import { detectHubCards, hubEntries, type HubCard } from "./registry";

const proc = (pid: number, comm: string, extra: Partial<ProcessInfo> = {}): ProcessInfo => ({
  pid,
  name: comm,
  comm,
  title_id: "",
  app_id: 0,
  memory_mib: 4,
  threads: 3,
  kind: "payload",
  ...extra,
});

const info = (id: string, display: string, hint: string | null, marker: string | null): PayloadInfo => ({
  id,
  display_name: display,
  role: "",
  description: "",
  repo_owner: "o",
  repo_name: "r",
  on_console_marker_path: marker,
  process_name_hint: hint,
  ports: [],
  autoload_priority: 0,
  autoload_delay_ms: 0,
  homepage: "",
});

const CATALOG: PayloadInfo[] = [
  info("kstuff-echostretch", "kstuff-lite (EchoStretch)", null, "/data/kstuff.elf"),
  info("kstuff-lite-drakmor", "kstuff-lite (drakmor — fpkg-optimized)", null, "/data/kstuff.elf"),
  info("shadowmountplus", "ShadowMount+", "shadowmountplus", "/data/shadowmount/debug.log"),
  info("payload-manager", "Payload Manager", null, "/data/pldmgr/pldmgr_config.txt"),
];
const ENTRIES = hubEntries(CATALOG);

const saved = (filename: string) => ({
  filename,
  path: `/data/pldmgr/payloads/local/${savedKey(filename)}/${filename}`,
});

// The #846 console: Payload Manager keeps four payloads, three of them run, and Kraken Mount
// runs as elfldr's generic payload.elf.
const PROCS: ProcessInfo[] = [
  proc(140, "ps5upload.elf", { is_self: true }),
  proc(106, "elfldr.elf"),
  proc(104, "syslang.elf"),
  proc(103, "payload.elf"),
  proc(102, "kstuff.elf"),
  proc(90, "pldmgr.elf"),
];
const SAVED = [
  saved("kstuff-1.13-fpkg.elf"),
  saved("kraken-mount.elf"),
  saved("syslang-zh-Hans-0.3.elf"),
  saved("AnyPad-PS5-0.9.1-beta.elf"),
];

function cardsFor(processes: ProcessInfo[], krakenAnswers = true): HubCard[] {
  return detectHubCards({
    entries: ENTRIES,
    processes,
    markers: new Set(),
    answering: new Set(krakenAnswers ? ["kraken-mount", "payload-manager"] : ["payload-manager"]),
  });
}

const byFile = (m: Map<string, HubCard>) =>
  Object.fromEntries([...m].map(([path, c]) => [path.split("/").pop(), [c.id, c.processes[0]?.pid]]));

describe("Payload Manager saved rows ↔ running cards", () => {
  it("matches the #846 console: kstuff by family, Kraken Mount on payload.elf, syslang by name", () => {
    const cards = cardsFor(PROCS);
    const m = matchSavedToCards(SAVED, cards, ENTRIES);
    expect(byFile(m)).toEqual({
      "kstuff-1.13-fpkg.elf": ["kstuff-echostretch", 102],
      "kraken-mount.elf": ["kraken-mount", 103],
      "syslang-zh-Hans-0.3.elf": ["proc:syslang", 104],
    });
  });

  it("pairs an exact file name with its process", () => {
    const cards = cardsFor([proc(129, "shadowmountplus.elf"), proc(96, "nanodns.elf")]);
    const m = matchSavedToCards([saved("shadowmountplus.elf"), saved("nanodns.elf")], cards, ENTRIES);
    expect(byFile(m)).toEqual({
      "shadowmountplus.elf": ["shadowmountplus", 129],
      "nanodns.elf": ["nanodns", 96],
    });
  });

  it("knows ShadowMount+'s short name as a whole word only", () => {
    const cards = cardsFor([proc(129, "shadowmountplus.elf")]);
    expect(byFile(matchSavedToCards([saved("smp-1.7.elf")], cards, ENTRIES))).toEqual({
      "smp-1.7.elf": ["shadowmountplus", 129],
    });
    expect(matchSavedToCards([saved("smpx.elf")], cards, ENTRIES).size).toBe(0);
  });

  it("prefers an exact name over a family one", () => {
    const cards = cardsFor([proc(102, "kstuff.elf")]);
    const m = matchSavedToCards([saved("kstuff-old.elf"), saved("kstuff.elf")], cards, ENTRIES);
    expect(byFile(m)).toEqual({ "kstuff.elf": ["kstuff-echostretch", 102] });
  });

  it("pairs nothing when two saved files could be the same process", () => {
    const cards = cardsFor([proc(102, "kstuff.elf")]);
    const m = matchSavedToCards([saved("kstuff-1.13.elf"), saved("kstuff-lite-fpkg.elf")], cards, ENTRIES);
    expect(m.size).toBe(0);
  });

  it("never claims a generic payload.elf by name, nor Payload Manager's own card", () => {
    // Kraken Mount's API is silent: payload.elf stays its own card, nobody's.
    const cards = cardsFor([proc(103, "payload.elf"), proc(90, "pldmgr.elf")], false);
    const m = matchSavedToCards([saved("payload.elf"), saved("pldmgr.elf"), saved("kraken-mount.elf")], cards, ENTRIES);
    expect(m.size).toBe(0);
  });

  it("matches a running payload only, not one found installed", () => {
    const cards = detectHubCards({
      entries: ENTRIES,
      processes: [],
      markers: new Set(["/data/shadowmount/debug.log"]),
    });
    expect(matchSavedToCards([saved("shadowmountplus.elf")], cards, ENTRIES).size).toBe(0);
  });
});

describe("cards once the saved list is known", () => {
  it("hides matched cards, keeps the rest under Also running", () => {
    const cards = cardsFor(PROCS);
    const g = groupCards(cards, matchSavedToCards(SAVED, cards, ENTRIES));
    expect([...g.hidden].sort()).toEqual(["kraken-mount", "kstuff-echostretch", "proc:syslang"]);
    expect(g.top.map((c) => c.id)).toEqual(["payload-manager"]);
    expect(g.also.map((c) => c.id)).toEqual(["proc:elfldr"]);
  });

  it("keeps a matched card that has its own panel at the top", () => {
    const cards = cardsFor([proc(129, "shadowmountplus.elf"), proc(90, "pldmgr.elf")], false);
    const g = groupCards(cards, matchSavedToCards([saved("shadowmountplus.elf")], cards, ENTRIES));
    expect(g.hidden.size).toBe(0);
    expect(g.top.map((c) => c.id)).toEqual(["payload-manager", "shadowmountplus"]);
    expect(g.also).toEqual([]);
  });

  it("says who started each running card", () => {
    const cards = cardsFor([...PROCS, proc(150, "ps5u-convert")]);
    const m = matchSavedToCards(SAVED, cards, ENTRIES);
    const ids = new Set([...m.values()].map((c) => c.id));
    const origin = (id: string, loader: number | null = null) => {
      const c = cards.find((x) => x.id === id)!;
      return originOf(c, ids.has(c.id), loader);
    };
    expect(origin("kstuff-echostretch")).toBe("pm");
    expect(origin("payload-manager")).toBe("console");
    expect(origin("proc:elfldr")).toBe("loader");
    expect(origin("proc:elfldr", 106)).toBe("loader-ours");
    expect(origin("proc:elfldr", 999)).toBe("loader");
    expect(origin("proc:ps5u-convert")).toBe("ours");
  });

  it("knows PS5Upload's own processes", () => {
    for (const n of ["ps5upload.elf", "ps5upload-installer.elf", "ps5u-vinput.elf", "ps5u-extract", "ps5u-convert"]) {
      expect(isOurProcess(proc(1, n))).toBe(true);
    }
    for (const n of ["kstuff.elf", "elfldr.elf", "payload.elf", "ps5debug.elf"]) {
      expect(isOurProcess(proc(1, n))).toBe(false);
    }
  });
});
