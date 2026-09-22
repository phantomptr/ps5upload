import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * How a link install should fetch its package, remembered per console.
 *
 * Unlike the settings in `installSettings.ts`, this one is PER HOST: two
 * consoles can sit behind different links, and the right answer follows the
 * link, not the user.
 *
 *  - `direct`   the PS5 downloads the URL itself, through the DPI daemon.
 *               Measured at ~90 MB/s from a LAN origin on FW 5.10, with this
 *               computer serving nothing, so it can be closed once the
 *               install starts. Two limits: the console must be able to reach
 *               the link itself, and its installer refuses a link longer than
 *               127 bytes (hardware-measured: 127 accepted, 128 refused with
 *               0x80A30003).
 *  - `stream`   this computer downloads over many connections and re-serves
 *               to the console as it asks, keeping nothing on disk. Measured
 *               at 108 MB/s median on the same package and hardware, so it is
 *               the fastest of the three here. It holds the link open for the
 *               whole install, so an expiring link or a sleeping computer
 *               takes the install with it.
 *  - `download` this computer downloads the whole package to disk first, then
 *               installs it as a local file. The slowest wall-clock of the
 *               three (two legs, one after the other) and it needs room for
 *               the package, but it is the only one that survives a link that
 *               dies mid-install: the download can be retried on its own, and
 *               the install afterwards never touches the network.
 */
export type LinkInstallMode = "direct" | "stream" | "download";

const KEY_MODE = "ps5upload.link_install_mode";
const KEY_INSECURE = "ps5upload.link_install_insecure";

/** Default for a console we have never installed a link on.
 *
 * `stream`, despite `direct` being simpler, for two reasons that outweigh
 * simplicity:
 *
 *  - It is what every existing install already does. Flipping the default
 *    would change behaviour under people who never asked for it.
 *  - Direct cannot be verified. `pkg_dpi_install` returns as soon as the
 *    console ACCEPTS the URL, so if the console cannot actually reach it we
 *    would report "downloading" and nothing would happen. Stream is tracked
 *    byte by byte, and works even when the URL is reachable only from this
 *    computer.
 *
 * It is also the fastest measured of the three (108 MB/s against direct's 90
 * on a LAN origin). The other two are offered, explained, and remembered once
 * chosen — they are just not imposed. */
const DEFAULT_MODE: LinkInstallMode = "stream";

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
      // "accelerated" was this mode's name before the download-to-disk
      // option existed; a stored preference must keep meaning what the
      // person chose rather than silently reverting to the default.
      const migrated = v === "accelerated" ? "stream" : v;
      if (valid(migrated)) out[host] = migrated;
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
  v === "direct" || v === "stream" || v === "download";
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
