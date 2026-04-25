import { create } from "zustand";

/** TCP port of the PS5 ELF loader. Constant — every common PS5
 *  homebrew loader binds this port, so it's not a user-configurable
 *  setting. Kept at module scope so the Connection
 *  UI can show it in copy without round-tripping through store state. */
export const PS5_LOADER_PORT = 9021;
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
  /** Version string the payload reports over STATUS. null = payload not
   *  reachable, or running a pre-2.1 build that doesn't report version. */
  payloadVersion: string | null;
  /** PS5 kernel build string (from `kern.version` sysctl). Useful for
   *  cross-referencing against psdevwiki firmware build tables. null if
   *  payload hasn't replied yet or is running an older build. */
  ps5Kernel: string | null;
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
        "payloadStatus" | "engineStatus" | "payloadVersion" | "ps5Kernel"
      >
    >
  ) => void;
  setStep1: (step: ConnStep, msg: string) => void;
  setStep2: (step: ConnStep, msg: string) => void;
}

export const useConnectionStore = create<ConnectionState>((set) => ({
  host: "192.168.137.2",
  engineStatus: "unknown",
  payloadStatus: "unknown",
  payloadVersion: null,
  ps5Kernel: null,
  step1: "idle",
  step1Msg: "Enter your PS5's address and check",
  step2: "idle",
  step2Msg: "Payload not loaded yet",
  setHost: (host) => set({ host }),
  setStatus: (patch) => set(patch),
  setStep1: (step1, step1Msg) => set({ step1, step1Msg }),
  setStep2: (step2, step2Msg) => set({ step2, step2Msg }),
}));
