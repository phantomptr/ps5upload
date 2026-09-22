import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * How a link install should fetch its package, remembered per console.
 *
 * Unlike the settings in `installSettings.ts`, this one is PER HOST: two
 * consoles can sit behind different links, and the right answer follows the
 * link, not the user. A PS5 next to a fast local mirror wants `direct`; one
 * pulling from a slow distant host wants `accelerated`.
 *
 *  - `direct`      the PS5 downloads the URL itself, through the DPI daemon.
 *                  Measured at 114 MB/s from a LAN origin on FW 5.10, with
 *                  this computer serving nothing. The console opens only two
 *                  connections, so it loses on a slow or distant source.
 *  - `accelerated` this computer downloads over many connections and re-serves
 *                  to the console. Wins wherever connection count is the only
 *                  lever, at the cost of keeping the computer awake.
 */
export type LinkInstallMode = "direct" | "accelerated";

const KEY_MODE = "ps5upload.link_install_mode";
const KEY_INSECURE = "ps5upload.link_install_insecure";

/** Default for a console we have never installed a link on.
 *
 * `accelerated`, despite `direct` being faster on a healthy source, for two
 * reasons that outweigh speed:
 *
 *  - It is what every existing install already does. Flipping the default
 *    would change behaviour under people who never asked for it.
 *  - Direct cannot yet be verified. `pkg_dpi_install` returns as soon as the
 *    console ACCEPTS the URL, so if the console cannot actually reach it we
 *    would report "downloading" and nothing would happen. Accelerated is
 *    tracked byte by byte, and works even when the URL is reachable only
 *    from this computer.
 *
 * Direct is offered, explained, and remembered once chosen — it is just not
 * imposed. */
const DEFAULT_MODE: LinkInstallMode = "accelerated";

/** Host → value maps, persisted as JSON.
 *
 * Every access is wrapped: in the browser build the app runs in an insecure
 * context where storage reads can throw outright, and a thrown preference
 * must never stop someone installing a package. */
function loadMap<T>(key: string, valid: (v: unknown) => v is T): Record<string, T> {
  if (typeof window === "undefined") return {};
  try {
    const raw = safeGetItem(key);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    const out: Record<string, T> = {};
    for (const [host, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (valid(v)) out[host] = v;
    }
    return out;
  } catch {
    return {};
  }
}

function saveMap(key: string, value: Record<string, unknown>): void {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(key, JSON.stringify(value));
  } catch {
    // Storage is unavailable or full. The choice still applies for this
    // session; only its persistence is lost.
  }
}

const isMode = (v: unknown): v is LinkInstallMode =>
  v === "direct" || v === "accelerated";
const isBool = (v: unknown): v is boolean => typeof v === "boolean";

interface LinkInstallPrefsState {
  modes: Record<string, LinkInstallMode>;
  insecure: Record<string, boolean>;
  /** The mode chosen for this console, or the default if never chosen. */
  modeFor: (host: string) => LinkInstallMode;
  setMode: (host: string, mode: LinkInstallMode) => void;
  /** Whether to skip certificate checks when THIS COMPUTER downloads.
   *  Always false for an unknown host — never sticky-on by accident. */
  insecureFor: (host: string) => boolean;
  setInsecure: (host: string, on: boolean) => void;
}

export const useLinkInstallPrefs = create<LinkInstallPrefsState>((set, get) => ({
  modes: loadMap(KEY_MODE, isMode),
  insecure: loadMap(KEY_INSECURE, isBool),
  modeFor: (host) => get().modes[host] ?? DEFAULT_MODE,
  setMode: (host, mode) => {
    if (!host) return;
    const modes = { ...get().modes, [host]: mode };
    saveMap(KEY_MODE, modes);
    set({ modes });
  },
  insecureFor: (host) => get().insecure[host] === true,
  setInsecure: (host, on) => {
    if (!host) return;
    const insecure = { ...get().insecure, [host]: on };
    saveMap(KEY_INSECURE, insecure);
    set({ insecure });
  },
}));
