// "On your PS5": which payloads are on the console, as cards. Pure: the panel feeds it the
// catalogue, the process list and which marker files exist, and renders what comes back.
//
// A payload is "running" when a process with its name is up, "installed" when its marker
// file is on the console. Payloads show up in the process list under their ELF's file name
// ("shadowmountplus.elf", "nanodns.elf", "pldmgr.elf"; measured on FW 13.60), so names are
// compared without case or extension. A payload that never names itself shows as the loader
// called it ("payload.elf" from elfldr): such a payload is found by other evidence, the pid
// it keeps in a lock file (Hush's /data/hush/pid.lock), passed in as `pids`.

import type { PayloadInfo, ProcessInfo } from "../../../api/ps5";

/** The payloads with a panel of their own on the card ("Manage"). Adding one = a registry
 *  entry here plus its panel in OnPs5Panel; never a new tab. */
export type IntegrationId = "shadowmountplus" | "nanodns" | "payload-manager" | "hearbridge" | "hush";

export const INTEGRATION_IDS: readonly IntegrationId[] = [
  "shadowmountplus",
  "nanodns",
  "payload-manager",
  "hearbridge",
  "hush",
];

export function isIntegration(id: string): id is IntegrationId {
  return (INTEGRATION_IDS as readonly string[]).includes(id);
}

/** What an integration's card summary learned from its own API. */
export interface IntegrationReport {
  version?: string | null;
  /** The port its web page answered on, when that differs from the catalogue's. */
  webPort?: number | null;
}

/** What the hub needs to know about one payload. */
export interface HubEntry {
  id: string;
  name: string;
  /** Process names it runs under (any case, with or without ".elf"). */
  processNames: string[];
  /** A file only it leaves on the console. */
  marker: string | null;
  /** Its web page's port, if it has one. */
  webPort: number | null;
  isCustom: boolean;
}

/** Ports the catalogue lists that serve a web page (diagnostics::known_port_is_web). */
const WEB_PORTS = new Set([8080, 8084, 8090]);

/** Process names the catalogue doesn't carry (it has no hint for these), measured on the
 *  console's process list. */
const EXTRA_PROCESS_NAMES: Record<string, string[]> = {
  // HyperHEN's built-in ShadowMount+ engine (engine smp.rs is_mount_process).
  shadowmountplus: ["hyperhen_mount", "hyperhen_automount"],
  "payload-manager": ["pldmgr"],
  "kstuff-echostretch": ["kstuff"],
  "kstuff-lite-drakmor": ["kstuff"],
};

/** The integrations as the browser build knows them: it has no catalogue (that lives in the
 *  desktop app), so these stand in for it. Mirrors commands/payloads.rs. */
export const BUILTIN_ENTRIES: readonly HubEntry[] = [
  {
    id: "shadowmountplus",
    name: "ShadowMount+",
    processNames: ["shadowmountplus", "hyperhen_mount", "hyperhen_automount"],
    marker: "/data/shadowmount/debug.log",
    webPort: null,
    isCustom: false,
  },
  {
    id: "nanodns",
    name: "nanoDNS",
    processNames: ["nanodns"],
    marker: "/data/nanodns/nanodns.ini",
    webPort: null,
    isCustom: false,
  },
  {
    id: "payload-manager",
    name: "Payload Manager",
    processNames: ["pldmgr"],
    marker: "/data/pldmgr/pldmgr_config.txt",
    webPort: 8084,
    isCustom: false,
  },
  {
    id: "hearbridge",
    name: "HearBridge",
    processNames: ["hearbridge"],
    marker: "/data/hearbridge/hearbridge.log",
    webPort: 8090,
    isCustom: false,
  },
  {
    id: "hush",
    name: "Hush",
    processNames: ["hush"],
    marker: "/data/hush/config.ini",
    webPort: null,
    isCustom: false,
  },
];

export function entryFromCatalog(p: PayloadInfo): HubEntry {
  const names = [
    ...(p.process_name_hint ? [p.process_name_hint] : []),
    ...(EXTRA_PROCESS_NAMES[p.id] ?? []),
  ];
  return {
    id: p.id,
    name: p.display_name,
    processNames: names,
    marker: p.on_console_marker_path,
    webPort: p.ports.find((port) => WEB_PORTS.has(port)) ?? null,
    isCustom: !!p.is_custom,
  };
}

/** The entries to detect: the catalogue when there is one, plus any integration it lacks. */
export function hubEntries(catalog: readonly PayloadInfo[] | null): HubEntry[] {
  const out = (catalog ?? []).map(entryFromCatalog);
  for (const b of BUILTIN_ENTRIES) if (!out.some((e) => e.id === b.id)) out.push(b);
  return out;
}

export function normProcName(s: string): string {
  return s.trim().toLowerCase().replace(/\.(elf|bin)$/, "");
}

export type HubState = "running" | "installed" | "unknown";

export interface HubCard {
  /** The catalogue id, or `proc:<name>` for a payload the catalogue doesn't know. */
  id: string;
  /** Every catalogue entry this card stands for (two kstuff builds share one process name). */
  entryIds: string[];
  name: string;
  state: HubState;
  /** Its running processes (empty when installed but not running). */
  processes: ProcessInfo[];
  webPort: number | null;
  integration: IntegrationId | null;
  isCustom: boolean;
}

export interface DetectInput {
  entries: readonly HubEntry[];
  processes: readonly ProcessInfo[];
  /** Marker paths found on the console. */
  markers: ReadonlySet<string>;
  /** Integrations whose own API answered (Payload Manager on :8084, HearBridge): running
   *  even when the process list hasn't been read yet. */
  answering?: ReadonlySet<string>;
  /** Processes known to be an entry's by other evidence than their name (entry id → pids). */
  pids?: ReadonlyMap<string, readonly number[]>;
  /** A card to show even when nothing was found (a deep link opened it). */
  forceShow?: string | null;
}

/** One card per payload that is running or installed, then one per running payload the
 *  catalogue doesn't know. Running before installed, integrations first, then by name. */
export function detectHubCards({
  entries,
  processes,
  markers,
  answering,
  pids,
  forceShow,
}: DetectInput): HubCard[] {
  const claimed = new Set<number>();
  const byEvidence = new Map<string, HubCard>();
  const cards: HubCard[] = [];
  for (const e of entries) {
    const names = new Set(e.processNames.map(normProcName).filter(Boolean));
    const own = pids?.get(e.id) ?? [];
    const procs = processes.filter(
      (p) =>
        !p.is_self &&
        (names.has(normProcName(p.comm)) || names.has(normProcName(p.name)) || own.includes(p.pid)),
    );
    const integration = isIntegration(e.id) ? e.id : null;
    const answered = !!answering?.has(e.id);
    const installed = !!e.marker && markers.has(e.marker);
    const forced = forceShow === e.id;
    if (procs.length === 0 && !installed && !answered && !forced) continue;
    for (const p of procs) claimed.add(p.pid);
    const state: HubState = procs.length > 0 || answered ? "running" : installed ? "installed" : "unknown";
    // Two catalogue entries seen through the same process or marker are one thing on the
    // console (kstuff's two builds both run as kstuff.elf): one card naming both.
    const evidence = procs.length
      ? `p:${procs.map((p) => p.pid).sort((a, b) => a - b).join(",")}`
      : installed && !answered
        ? `m:${e.marker}`
        : `id:${e.id}`;
    const same = byEvidence.get(evidence);
    if (same && !integration && !same.integration) {
      same.entryIds.push(e.id);
      same.name = `${same.name} / ${e.name}`;
      continue;
    }
    const card: HubCard = {
      id: e.id,
      entryIds: [e.id],
      name: e.name,
      state,
      processes: procs,
      webPort: e.webPort,
      integration,
      isCustom: e.isCustom,
    };
    byEvidence.set(evidence, card);
    cards.push(card);
  }
  // Payloads nobody catalogued, seen running (one card per name).
  const unknown = new Map<string, HubCard>();
  for (const p of processes) {
    if (p.kind !== "payload" || p.is_self || claimed.has(p.pid)) continue;
    const label = p.comm || p.name;
    const key = normProcName(label);
    if (!key) continue;
    const card = unknown.get(key);
    if (card) {
      card.processes.push(p);
      continue;
    }
    unknown.set(key, {
      id: `proc:${key}`,
      entryIds: [],
      name: label,
      state: "running",
      processes: [p],
      webPort: null,
      integration: null,
      isCustom: true,
    });
  }
  const rank = (c: HubCard) =>
    (c.state === "running" ? 0 : c.state === "installed" ? 2 : 4) + (c.integration ? 0 : 1);
  return [...cards, ...unknown.values()].sort(
    (a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name),
  );
}

/** Group marker paths by folder, so each folder is listed once. */
export function markerFolders(entries: readonly HubEntry[]): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const e of entries) {
    const m = e.marker?.replace(/\/+$/, "");
    if (!m) continue;
    const slash = m.lastIndexOf("/");
    if (slash < 0) continue;
    const dir = slash === 0 ? "/" : m.slice(0, slash);
    const list = out.get(dir) ?? [];
    if (!list.includes(m)) list.push(m);
    out.set(dir, list);
  }
  return out;
}
