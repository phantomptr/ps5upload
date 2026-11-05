// What "Check" finds on a console: one answer for the Connection screen and the setup wizard,
// which used to carry their own copies and disagreed (the wizard had no rest mode, no running
// helper and no reason why a probe failed).

import { ddpStatus, payloadCheck, portProbe } from "../api/ps5";
import { localNetworkBlocked } from "./androidLocalNetwork";
import { PAYLOAD_MANAGER_PORT, PS5_LOADER_PORT } from "./addr";
import { checkSaysRest, powerFromCheck } from "./consolePower";
import { localNetworkHintFor } from "./localNetworkHint";
import { isIOS } from "./platform";
import type { Translator } from "../state/lang";

type HelperCheck = Awaited<ReturnType<typeof payloadCheck>>;

export type ConsoleCheck =
  /** The loader answers. `helper`: a helper is running too (paired or not). */
  | { kind: "loader"; helper: HelperCheck | null; helperRunning: boolean }
  /** No loader, but a helper is running: some loaders do not keep :9021 open after boot. */
  | { kind: "helper"; helper: HelperCheck }
  /** No loader and no helper, but Payload Manager can launch the helper (#344). */
  | { kind: "payload-manager"; error: string | null }
  /** Asleep: nothing listens until it wakes. */
  | { kind: "rest" }
  /** The console is on and answers, but nothing there can take the helper. */
  | { kind: "no-loader"; error: string | null }
  /** Nothing answered: off, another network, a blocked app, a wrong address. */
  | { kind: "unreachable"; error: string | null };

export interface ConsoleCheckDeps {
  probe: (host: string, port: number) => Promise<{ open: boolean; error: string | null }>;
  helper: (host: string) => Promise<HelperCheck>;
  ddpCode: (host: string) => Promise<number>;
}

const liveDeps: ConsoleCheckDeps = {
  probe: portProbe,
  helper: payloadCheck,
  ddpCode: (host) => ddpStatus(host).then((d) => d.code),
};

/** Never throws: a failed probe is an answer (the web UI's goes through an engine that can be
 *  away, and a throw here left Check spinning for good). */
export async function checkConsole(
  target: string,
  deps: ConsoleCheckDeps = liveDeps,
): Promise<ConsoleCheck> {
  const probe = (port: number) =>
    deps.probe(target, port).catch((e: unknown) => ({
      open: false,
      error: e instanceof Error ? e.message : String(e),
    }));
  const loader = await probe(PS5_LOADER_PORT);
  const helper = await deps.helper(target).catch(() => null);
  // A console that answers but wants pairing has a helper running.
  const helperRunning = !!helper?.reachable || helper?.session === "needs_pairing";
  if (loader.open) return { kind: "loader", helper, helperRunning };
  if (helper && helperRunning) return { kind: "helper", helper };
  if (await checkSaysRest(helper, () => deps.ddpCode(target))) return { kind: "rest" };
  // DDP said awake, or the helper's port refused rather than stayed silent: the console is on.
  const on = powerFromCheck(helper) === "awake" || helper?.reason === "helper_not_ava1";
  if (!on) return { kind: "unreachable", error: loader.error };
  const manager = await probe(PAYLOAD_MANAGER_PORT);
  return manager.open
    ? { kind: "payload-manager", error: loader.error }
    : { kind: "no-loader", error: loader.error };
}

/** What a failed check says: where to look when nothing answered, what to load when only the
 *  loader is missing. */
export function closedPortText(
  tr: Translator,
  target: string,
  check: Extract<ConsoleCheck, { kind: "no-loader" | "unreachable" }>,
  userAgent: string = typeof navigator !== "undefined" ? navigator.userAgent : "",
): string {
  // Append the probe's own reason. Without it a name that simply didn't
  // resolve reads identically to a console that isn't jailbroken yet,
  // and the user has no way to tell the two apart (#272).
  const base = tr(
    "connection_port_closed",
    { port: PS5_LOADER_PORT, host: target },
    "Port {port} is not open on {host}",
  );
  const detail = check.error ? `${base} — ${check.error}` : base;
  if (check.kind === "no-loader") {
    return tr(
      "connection_loader_down",
      { error: detail },
      "The PS5's loader (port 9021) isn't running. Load elfldr on the PS5 (your jailbreak or autoloader does it), then send the helper again. ({error})",
    );
  }
  if (localNetworkBlocked()) {
    return tr(
      "connection_android_local_network_hint",
      undefined,
      "Android is blocking PS5Upload from your local network. Tap Allow at the top, then check again.",
    );
  }
  // "No route to host" at once, on a Mac, with the console known to be up from another
  // computer, is what a denied Local Network permission looks like: say where it is.
  const networkHint = localNetworkHintFor(check.error, userAgent, isIOS());
  if (networkHint === "ios") {
    return `${detail}. ${tr("connection_ios_local_network_hint", undefined, "iOS may be blocking this app from your local network. Open Settings → PS5Upload, turn on Local Network, then check again.")}`;
  }
  if (networkHint === "mac") {
    return `${detail}. ${tr("connection_mac_local_network_hint", undefined, "macOS may be blocking this app from your local network. Open System Settings → Privacy & Security → Local Network, allow PS5Upload, then check again.")}`;
  }
  // A VPN on this computer sends the console's address out through the tunnel: the
  // likeliest cause when everything else is right (reported twice on Discord).
  return `${detail}. ${tr("connection_vpn_hint", undefined, "If a VPN is on, turn it off (or allow local network access in it), then check again.")}`;
}

/** Step 1's line when Payload Manager stands in for the loader. */
export function payloadManagerReadyText(tr: Translator): string {
  return tr(
    "connection_payload_manager_ready",
    undefined,
    "Port 9021 is closed, but Payload Manager is running. The app can send the helper through it.",
  );
}
