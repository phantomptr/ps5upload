/** Wake-from-standby: turning console state + stored credential into UI state.
 *
 *  Kept out of the component so the gating — which button shows, when setup
 *  is offered — is testable without rendering, and so the rules live in one
 *  place instead of scattered across JSX conditionals. */

/** What the console last told us over the discovery protocol. */
export type PowerState = "awake" | "standby" | "offline";

/** DDP reply code → power state. 200 = awake, 620 = standby; anything else
 *  (including no reply, which surfaces as `undefined`) means we cannot see
 *  the console, which is different from it being off. */
export function powerStateFromDdp(code: number | undefined): PowerState {
  if (code === 200) return "awake";
  if (code === 620) return "standby";
  return "offline";
}

export interface WakeUi {
  /** The console is asleep and we have a credential — waking is possible. */
  showWakeButton: boolean;
  /** No credential stored yet, so offer the setup panel. */
  showSetup: boolean;
  /** Automatic setup registers over the network, which needs the console
   *  awake — the opposite of when waking is useful, so it is only offered
   *  while the console is on. */
  canAutoSetup: boolean;
}

export function wakeUi(state: PowerState, hasCredential: boolean): WakeUi {
  return {
    showWakeButton: state === "standby" && hasCredential,
    showSetup: !hasCredential,
    canAutoSetup: state === "awake",
  };
}

/** A wake credential is the registration key rendered as a decimal number.
 *  The console ignores a malformed one silently, so a paste is checked here
 *  before it is stored: digits only, and within the u64 range a real key
 *  occupies. This rejects an accidental paste of the raw hex key or a whole
 *  config line, which would otherwise look saved but never wake anything. */
export function isValidWakeCredential(value: string): boolean {
  const t = value.trim();
  if (!/^\d+$/.test(t)) return false;
  try {
    const n = BigInt(t);
    return n > 0n && n <= 18446744073709551615n; // u64 max
  } catch {
    return false;
  }
}

/** The console settings that must be on, or wake fails with no feedback.
 *  Exact Sony wording and menu paths — getting these subtly wrong is worse
 *  than useless, because the user follows them to a setting that isn't there.
 *  Data, not JSX, so the copy is translated through the normal i18n keys and
 *  the list can be rendered the same way in more than one place. */
export interface WakeRequirement {
  /** i18n key for the setting's exact name. */
  labelKey: string;
  labelFallback: string;
  /** i18n key for where it lives in the console's menus. */
  pathKey: string;
  pathFallback: string;
}

export const WAKE_REQUIREMENTS: WakeRequirement[] = [
  {
    labelKey: "power_wake_req_remoteplay",
    labelFallback: "Enable Remote Play",
    pathKey: "power_wake_req_remoteplay_path",
    pathFallback: "Settings › System › Remote Play",
  },
  {
    labelKey: "power_wake_req_stayconnected",
    labelFallback: "Stay Connected to the Internet",
    pathKey: "power_wake_req_restmode_path",
    pathFallback: "Settings › System › Power Saving › Features Available in Rest Mode",
  },
  {
    labelKey: "power_wake_req_network",
    labelFallback: "Enable Turning On PS5 from Network",
    pathKey: "power_wake_req_restmode_path",
    pathFallback: "Settings › System › Power Saving › Features Available in Rest Mode",
  },
];
