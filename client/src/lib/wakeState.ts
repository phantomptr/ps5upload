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
  /** The full session keys are stored, so the wake can sign the user in
   *  rather than land at user-select. Changes the wake action's meaning. */
  canSignIn: boolean;
}

export function wakeUi(
  state: PowerState,
  hasCredential: boolean,
  hasSessionKeys = false,
): WakeUi {
  return {
    // Session keys imply a wake credential (it derives from the regist key),
    // so either is enough to enable the wake button.
    showWakeButton: state === "standby" && (hasCredential || hasSessionKeys),
    showSetup: !hasCredential && !hasSessionKeys,
    canAutoSetup: state === "awake",
    canSignIn: hasSessionKeys,
  };
}

/** u64 max, as the decimal string the console's credential is bounded by. */
const U64_MAX_DEC = "18446744073709551615";

/* The two helpers below deliberately avoid BigInt.
 *
 * The Vite build target is safari13 (~ES2019) so the app survives old Android
 * System WebViews (see vite.config.ts). BigInt is ES2020 and, unlike `?.` or
 * `??=`, it CANNOT be down-levelled — rolldown passes the literals through and
 * warns TOLERATED_TRANSFORM. On a WebView without BigInt that is a parse-time
 * SyntaxError for the whole chunk, so it would take out the entire bundle, not
 * just wake setup: exactly the "opens then terminates" failure the conservative
 * target exists to prevent. Plain string arithmetic is exact here and portable.
 */

/** Is a digits-only decimal string larger than u64 max? */
function decExceedsU64(dec: string): boolean {
  const d = dec.replace(/^0+/, "");
  if (d.length !== U64_MAX_DEC.length) return d.length > U64_MAX_DEC.length;
  // Equal length, digits only — lexicographic order is numeric order.
  return d > U64_MAX_DEC;
}

/** Hex string → decimal string, schoolbook base conversion over digit arrays.
 *  Caller guarantees `hex` is non-empty, lowercase, and at most 16 digits. */
function hexToDec(hex: string): string {
  const digits = [0]; // little-endian decimal digits
  for (let h = 0; h < hex.length; h++) {
    let carry = parseInt(hex[h], 16);
    for (let i = 0; i < digits.length; i++) {
      const v = digits[i] * 16 + carry;
      digits[i] = v % 10;
      carry = (v / 10) | 0;
    }
    while (carry > 0) {
      digits.push(carry % 10);
      carry = (carry / 10) | 0;
    }
  }
  return digits.reverse().join("").replace(/^0+(?=\d)/, "");
}

/** Which face the setup panel shows.
 *
 *  One value drives the whole panel so the states stay mutually exclusive —
 *  the old version derived each piece of UI from its own boolean and could
 *  show a "set up" button and a "you're set up" tick at the same time.
 *
 *  - `working`     — pairing is in flight; outranks everything so the panel
 *                    does not flicker between states as power state updates.
 *  - `done`        — wake AND sign-in are configured. Nothing left to do.
 *  - `wake-only`   — a credential but no session keys: wake lands on
 *                    user-select. Worth offering the upgrade.
 *  - `offer`       — nothing stored and the console is awake: one click away.
 *  - `unreachable` — nothing stored and we cannot reach the console. Pairing
 *                    registers over the network, so it genuinely cannot run;
 *                    say so rather than hide the button with no explanation.
 */
export type WakeSetupStage =
  | "working"
  | "done"
  | "wake-only"
  | "offer"
  | "unreachable";

export function wakeSetupStage(
  state: PowerState,
  hasCredential: boolean,
  hasSessionKeys: boolean,
  pairing: boolean,
): WakeSetupStage {
  if (pairing) return "working";
  // Session keys imply the credential (it derives from the regist key), so
  // they alone are a complete setup even if no credential was stored.
  if (hasSessionKeys) return "done";
  if (hasCredential) return "wake-only";
  return state === "awake" ? "offer" : "unreachable";
}

/** The wake credential (a decimal number) derived from the registration key.
 *
 *  The regist key is 16 hex-encoded bytes, NUL-padded; the credential is the
 *  ASCII text before the first NUL read as a hexadecimal number. This is the
 *  same derivation the console applies when it matches a WAKEUP, so storing
 *  the session keys is enough to also wake — no separate credential paste.
 *  Returns "" if the input is not a usable regist key. */
export function credentialFromRegistKeyHex(registKeyHex: string): string {
  const hex = registKeyHex.trim().toLowerCase();
  if (!/^[0-9a-f]+$/.test(hex) || hex.length % 2 !== 0) return "";
  // hex → bytes → text up to the first NUL.
  let text = "";
  for (let i = 0; i < hex.length; i += 2) {
    const byte = parseInt(hex.slice(i, i + 2), 16);
    if (byte === 0) break;
    text += String.fromCharCode(byte);
  }
  // That text is itself hex; parse it as a number.
  if (!/^[0-9a-f]+$/.test(text)) return "";
  const stripped = text.replace(/^0+/, "");
  // All zeros is not a usable credential (the old BigInt form rejected n <= 0).
  if (stripped === "") return "";
  // 16 hex digits is exactly u64 max, so a shorter-or-equal value is in range.
  if (stripped.length > 16) return "";
  return hexToDec(stripped);
}

/** A 16-byte key as exactly 32 hex characters. */
export function isValidSessionKey(hex: string): boolean {
  return /^[0-9a-fA-F]{32}$/.test(hex.trim());
}

/** A wake credential is the registration key rendered as a decimal number.
 *  The console ignores a malformed one silently, so a paste is checked here
 *  before it is stored: digits only, and within the u64 range a real key
 *  occupies. This rejects an accidental paste of the raw hex key or a whole
 *  config line, which would otherwise look saved but never wake anything. */
export function isValidWakeCredential(value: string): boolean {
  const t = value.trim();
  if (!/^\d+$/.test(t)) return false;
  if (t.replace(/^0+/, "") === "") return false; // zero
  return !decExceedsU64(t);
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
