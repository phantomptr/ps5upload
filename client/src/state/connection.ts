import { createContext, createElement, useContext, type ReactNode } from "react";
import { create } from "zustand";
import { hostOf } from "../lib/addr";
import type { SessionState } from "../lib/consoleSession";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** TCP port of the PS5 ELF loader. Constant — every common PS5
 *  homebrew loader binds this port, so it's not a user-configurable
 *  setting. Kept at module scope so the Connection
 *  UI can show it in copy without round-tripping through store state. */
export const PS5_LOADER_PORT = 9021;

/** localStorage key for the last-typed PS5 host. Persisted so a
 *  reload doesn't drop the user back to empty — the typical user
 *  has a single PS5 IP they reuse across sessions. Only the host
 *  is persisted; everything downstream of it (probe results,
 *  version, kernel) is re-derived on next probe rather than
 *  restored as stale data. */
const HOST_STORAGE_KEY = "ps5upload.host";
/** Empty default (2.11.0). Previously hardcoded `192.168.137.2`
 *  which is the USB-tether-on-Windows-ICS gateway — wrong for
 *  ~95% of users, who hit a confusing red "Check failed" on first
 *  Connection click because they didn't notice the field already
 *  had a non-empty value. Empty default lets the placeholder do
 *  its job and forces the user to read what they're typing. The
 *  Discover panel below the input remains the recommended onboarding. */
const DEFAULT_HOST = "";

function loadStoredHost(): string {
  if (typeof window === "undefined") return DEFAULT_HOST;
  try {
    const stored = safeGetItem(HOST_STORAGE_KEY);
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
    safeSetItem(HOST_STORAGE_KEY, host);
  } catch {
    // Best-effort — host persistence is nice-to-have.
  }
}

export type ProbeStatus = "up" | "down" | "unknown";

/**
 * Capabilities exposed by the ps5upload payload when loaded.
 *
 * These are features of OUR payload, not probes for external tools —
 * ps5upload ships its own ELF loader (port 9021), its own AVA1 transfer
 * runtime (port 9120), and its own disk-image mount path (via the PS5
 * kernel's LVD backend, not an external daemon). XMB title registration
 * remains deliberately out of scope; use a PS5-side installer for that.
 */
export type PayloadCapability =
  /** AVA1 transfer runtime on :9120 */
  | "transfer"
  /** Disk image mount via /dev/lvdctl */
  | "mount";

/** Persisted step-flow state for the Connection screen. Lives in the
 *  store (not component-local useState) so navigating to Upload and back
 *  doesn't reset the user's progress. Only the *settled* states live here
 *  — transient "busy/fail" states stay local to the component because
 *  they're tied to in-flight probes, not to "has the user connected?". */
export type ConnStep = "idle" | "ok";

/**
 * Per-console live runtime, keyed by bare host. The status poller fans out
 * over every roster console and writes each one's slot here, so the tab strip
 * can show a live status dot + version for EVERY console at once — not just the
 * active one. The flat top-level fields below remain a mirror of the *active*
 * console's runtime so the ~21 screens that read `s.payloadStatus` etc. keep
 * working unchanged (they always view the active tab). Phase 3 re-pins screens
 * to read `runtimeByHost[theirHost]` directly; until then the mirror bridges.
 */
export interface HostRuntime {
  payloadStatus: ProbeStatus;
  payloadVersion: string | null;
  ps5Kernel: string | null;
  ucredElevated: boolean | null;
  priorInstance: string | null;
  /** The AVA1 session verdict from the same single probe that sets
   *  `payloadStatus`: connected, needs_pairing or down. There
   *  is one listener, so there is no second liveness flag to disagree
   *  with it. null = host not yet probed. */
  session: SessionState | null;
}

export const EMPTY_HOST_RUNTIME: HostRuntime = {
  payloadStatus: "unknown",
  payloadVersion: null,
  ps5Kernel: null,
  ucredElevated: null,
  priorInstance: null,
  session: null,
};

export interface ConnectionState {
  host: string;
  /** Per-console runtime, keyed by bare host (port-stripped). */
  runtimeByHost: Record<string, HostRuntime>;
  /** Our host-side engine sidecar (localhost:19113). */
  engineStatus: ProbeStatus;
  /** Last startup/probe diagnostic for the host-side engine. */
  engineError: string | null;
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
  /** Whether the payload's process-wide ucred elevation succeeded.
   *  true  = kernel R/W primitive available; privileged Sony APIs
   *          (BGFT IntDebug Register, ShellUI hooks, fakepkg install)
   *          will work.
   *  false = the loader didn't grant kernel R/W; install + most
   *          privileged ops will fail until the user loads via a
   *          jailbroken entry point (e.g. kstuff) that grants kernel R/W.
   *  null  = pre-2.2.52 payload that doesn't expose the field, or the
   *          probe hasn't returned yet. Renders as "—" in the UI.
   *  Set by Connection's payload probe + AppShell's polling tick. */
  ucredElevated: boolean | null;
  /** How the PREVIOUS payload instance ended, straight from STATUS_ACK:
   *  "clean" | "killed_externally" | "wedged" | "stale" | "replaced". null on payloads
   *  older than this field, which is indistinguishable from "unknown". */
  priorInstance: string | null;
  /** Mirror of the active console's AVA1 session state (see
   *  HostRuntime.session). */
  session: SessionState | null;
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
        | "engineError"
        | "payloadVersion"
        | "ps5Kernel"
        | "ucredElevated"
        | "priorInstance"
        | "payloadProbing"
        | "session"
      >
    >
  ) => void;
  setStep1: (step: ConnStep, msg: string) => void;
  setStep2: (step: ConnStep, msg: string) => void;
  /** Set a check step for `host` only while it is the selected console: a check that answers
   *  after a tab switch must not land on the other console's screen. */
  setStep1For: (host: string, step: ConnStep, msg: string) => void;
  setStep2For: (host: string, step: ConnStep, msg: string) => void;
  /** Write one console's live runtime (keyed by host). When `host` is the
   *  active console, the flat top-level fields are mirrored so the screens
   *  (which read the active tab) update too. Used by the fan-out poller. */
  setHostStatus: (host: string, patch: Partial<HostRuntime>) => void;
}

/** Map a runtime record onto the flat top-level fields the screens read. */
function mirrorRuntime(host: string, rt: HostRuntime) {
  return {
    payloadStatus: rt.payloadStatus,
    payloadStatusHost: rt.payloadStatus === "unknown" ? null : host,
    payloadVersion: rt.payloadVersion,
    ps5Kernel: rt.ps5Kernel,
    ucredElevated: rt.ucredElevated,
    priorInstance: rt.priorInstance,
    session: rt.session,
  };
}

const useLiveConnectionStore = create<ConnectionState>((set) => ({
  host: loadStoredHost(),
  runtimeByHost: {},
  engineStatus: "unknown",
  engineError: null,
  payloadStatus: "unknown",
  payloadStatusHost: null,
  payloadVersion: null,
  ps5Kernel: null,
  ucredElevated: null,
  priorInstance: null,
  session: null,
  payloadProbing: false,
  step1: "idle",
  step1Msg: "Enter your PS5's address and check",
  step2: "idle",
  step2Msg: "Payload not loaded yet",
  setHost: (host) =>
    set((s) => {
      persistHost(host);
      // Switching tabs: immediately reflect the new console's last-known
      // runtime (or unknowns) instead of leaving the previous console's
      // values on screen until the next poll — kills the stale-host window.
      const rt = s.runtimeByHost[hostOf(host) || "_"] ?? EMPTY_HOST_RUNTIME;
      // Reset the Connection screen's two-step check status too — those
      // fields are global, so leaving them showing console A's "✓ ok"
      // after switching to (unchecked) console B is misleading.
      return {
        host,
        ...mirrorRuntime(host, rt),
        step1: "idle" as const,
        step1Msg: "Enter your PS5's address and check",
        step2: "idle" as const,
        step2Msg: "Payload not loaded yet",
      };
    }),
  setStatus: (patch) => set(patch),
  setStep1: (step1, step1Msg) => set({ step1, step1Msg }),
  setStep2: (step2, step2Msg) => set({ step2, step2Msg }),
  setStep1For: (host, step1, step1Msg) =>
    set((s) => (hostOf(s.host) === hostOf(host) ? { step1, step1Msg } : {})),
  setStep2For: (host, step2, step2Msg) =>
    set((s) => (hostOf(s.host) === hostOf(host) ? { step2, step2Msg } : {})),
  setHostStatus: (host, patch) =>
    set((s) => {
      const key = hostOf(host) || "_";
      const next = { ...(s.runtimeByHost[key] ?? EMPTY_HOST_RUNTIME), ...patch };
      const activeKey = hostOf(s.host) || "_";
      return {
        runtimeByHost: { ...s.runtimeByHost, [key]: next },
        // Mirror to the flat fields only when this is the active console.
        ...(key === activeKey ? mirrorRuntime(host, next) : {}),
      };
    }),
}));

/** Hook: one console's live runtime (keyed by host). For the tab strip + any
 *  per-console status display. Returns the empty runtime if not yet probed. */
export function useHostRuntime(host: string): HostRuntime {
  return useConnectionStore(
    (s) => s.runtimeByHost[hostOf(host) || "_"] ?? EMPTY_HOST_RUNTIME,
  );
}

/**
 * The connection state as one console's screens see it.
 *
 * Each console the user has open keeps its own tree of screens (see App.tsx). Only one is on
 * show; the others are hidden, not unmounted, so what was typed or loaded in them is still
 * there on return. A hidden tree must not turn into the selected console: its screens read
 * this store for `host`, and would otherwise re-render as the other console. So while a
 * console is hidden its tree reads the state as it last was when that console was selected.
 *
 * `getState`, `setState` and `subscribe` are always the live store: code that runs outside
 * rendering (runners, callbacks) asks for the selected console on purpose.
 */
const lastStateByHost = new Map<string, ConnectionState>();
const remember = (s: ConnectionState) => lastStateByHost.set(s.host, s);
remember(useLiveConnectionStore.getState());
useLiveConnectionStore.subscribe(remember);

/** `host`'s connection state as it last was while that console was selected. */
export function connectionSnapshotFor(host: string): ConnectionState | null {
  return lastStateByHost.get(host) ?? null;
}

const FrozenConnection = createContext<ConnectionState | null>(null);

/** Wraps one console's tree of screens. `frozen`: that console is not the selected one. */
export function ConnectionScope({
  host,
  frozen,
  children,
}: {
  host: string;
  frozen: boolean;
  children: ReactNode;
}) {
  return createElement(
    FrozenConnection.Provider,
    { value: frozen ? connectionSnapshotFor(host) : null },
    children,
  );
}

/** Whether this screen belongs to a console that is not the selected one right now. */
export function useConnectionFrozen(): boolean {
  return useContext(FrozenConnection) !== null;
}

function useScopedConnectionStore(): ConnectionState;
function useScopedConnectionStore<T>(selector: (s: ConnectionState) => T): T;
function useScopedConnectionStore<T>(selector?: (s: ConnectionState) => T) {
  const frozen = useContext(FrozenConnection);
  const pick = selector ?? ((s: ConnectionState) => s as unknown as T);
  const live = useLiveConnectionStore(pick);
  return frozen ? pick(frozen) : live;
}

export const useConnectionStore = Object.assign(useScopedConnectionStore, {
  getState: useLiveConnectionStore.getState,
  getInitialState: useLiveConnectionStore.getInitialState,
  setState: useLiveConnectionStore.setState,
  subscribe: useLiveConnectionStore.subscribe,
});
