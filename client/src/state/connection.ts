import { create } from "zustand";

/** TCP port of the PS5 ELF loader. Constant — every common PS5
 *  homebrew loader binds this port, so it's not a user-configurable
 *  setting. Kept at module scope so the Connection
 *  UI can show it in copy without round-tripping through store state. */
export const PS5_LOADER_PORT = 9021;

/** localStorage key for the last-typed PS5 host. Persisted so a
 *  reload doesn't drop the user back to the hardcoded 192.168.137.2
 *  default — the typical user has a single PS5 IP they reuse across
 *  sessions. Only the host is persisted; everything downstream of
 *  it (probe results, version, kernel) is re-derived on next probe
 *  rather than restored as stale data. */
const HOST_STORAGE_KEY = "ps5upload.host";
const DEFAULT_HOST = "192.168.137.2";

function loadStoredHost(): string {
  if (typeof window === "undefined") return DEFAULT_HOST;
  try {
    const stored = window.localStorage.getItem(HOST_STORAGE_KEY);
    return stored && stored.trim() ? stored : DEFAULT_HOST;
  } catch {
    // localStorage unavailable (private mode, sandboxed iframe).
    // Fall through to the default — UX continues to work, the
    // user just retypes the IP each session.
    return DEFAULT_HOST;
  }
}

function persistHost(host: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(HOST_STORAGE_KEY, host);
  } catch {
    // Best-effort — host persistence is nice-to-have.
  }
}
/** TCP port our payload listens on for FTX2 transfers. Also constant. */
export const PS5_PAYLOAD_PORT = 9113;

export type ProbeStatus = "up" | "down" | "unknown";

/**
 * Capabilities exposed by the ps5upload payload when loaded.
 *
 * These are features of OUR payload, not probes for external tools —
 * ps5upload ships its own ELF loader (port 9021), its own FTX2 transfer
 * runtime (port 9113), and its own disk-image mount path (via the PS5
 * kernel's LVD backend, not an external daemon). XMB title registration
 * remains deliberately out of scope; use a PS5-side installer for that.
 */
export type PayloadCapability =
  /** FTX2 binary transfer runtime on :9113 */
  | "transfer"
  /** Disk image mount via /dev/lvdctl */
  | "mount";

/** Persisted step-flow state for the Connection screen. Lives in the
 *  store (not component-local useState) so navigating to Upload and back
 *  doesn't reset the user's progress. Only the *settled* states live here
 *  — transient "busy/fail" states stay local to the component because
 *  they're tied to in-flight probes, not to "has the user connected?". */
export type ConnStep = "idle" | "ok";

export interface ConnectionState {
  host: string;
  /** Our host-side engine sidecar (localhost:19113). */
  engineStatus: ProbeStatus;
  /** Our PS5-side payload runtime — transfer, mount, FS ops, hardware info. */
  payloadStatus: ProbeStatus;
  /** The host the most recent payload probe ran against. Allows
   *  consumers to ignore a stale `payloadStatus` from a previous host
   *  when the user has just typed a new IP — the AppShell probe
   *  re-fires on host change but there is a brief window where the
   *  prior probe's "up" result coexists with the new host string. */
  payloadStatusHost: string | null;
  /** Version string the payload reports over STATUS. null = payload not
   *  reachable, or running a pre-2.1 build that doesn't report version. */
  payloadVersion: string | null;
  /** PS5 kernel build string (from `kern.version` sysctl). Useful for
   *  cross-referencing against psdevwiki firmware build tables. null if
   *  payload hasn't replied yet or is running an older build. */
  ps5Kernel: string | null;
  /** True when a fresh payload-info probe is in flight and the
   *  currently-displayed payloadVersion / ps5Kernel may be stale.
   *  Set by Connection's handleSend on entry (the user just kicked
   *  off a Replace payload — anything we show is from BEFORE the new
   *  ELF booted, so flag it as stale until the next probe lands).
   *  Cleared by AppShell's polling tick after it writes a fresh
   *  version, AND by handleSend's own in-line probe so the wait time
   *  isn't gated on the next 10s tick. UI uses this to render a
   *  "rechecking…" badge so the user doesn't squint at numbers that
   *  may have been replaced under their feet. */
  payloadProbing: boolean;
  /** Step 1: loader-port reachability confirmed. */
  step1: ConnStep;
  step1Msg: string;
  /** Step 2: payload booted and responding on the management port. */
  step2: ConnStep;
  step2Msg: string;
  setHost: (host: string) => void;
  setStatus: (
    patch: Partial<
      Pick<
        ConnectionState,
        | "payloadStatus"
        | "payloadStatusHost"
        | "engineStatus"
        | "payloadVersion"
        | "ps5Kernel"
        | "payloadProbing"
      >
    >
  ) => void;
  setStep1: (step: ConnStep, msg: string) => void;
  setStep2: (step: ConnStep, msg: string) => void;
}

export const useConnectionStore = create<ConnectionState>((set) => ({
  host: loadStoredHost(),
  engineStatus: "unknown",
  payloadStatus: "unknown",
  payloadStatusHost: null,
  payloadVersion: null,
  ps5Kernel: null,
  payloadProbing: false,
  step1: "idle",
  step1Msg: "Enter your PS5's address and check",
  step2: "idle",
  step2Msg: "Payload not loaded yet",
  setHost: (host) => {
    persistHost(host);
    set({ host });
  },
  setStatus: (patch) => set(patch),
  setStep1: (step1, step1Msg) => set({ step1, step1Msg }),
  setStep2: (step2, step2Msg) => set({ step2, step2Msg }),
}));
