import { useState } from "react";

import { powerWake } from "../api/ps5";
import { useRosterStore, withConsolePrefix } from "../state/roster";
import { pushNotification } from "../state/notifications";
import { audit } from "../state/auditLog";
import { useTr } from "../state/lang";
import { hostOf } from "./addr";
import { useWakeSignInStore, wakeAndSignInAsking } from "./wakeSignIn";

/** The wake credentials stored for one console (by its address, not the selected profile). */
export function wakeCredentialsFor(
  profiles: Array<{
    id: string;
    host: string;
    wake_credential?: string | null;
    wake_regist_key?: string | null;
    wake_rp_key?: string | null;
  }>,
  host: string,
) {
  const key = hostOf(host) || host;
  const profile = profiles.find((p) => (hostOf(p.host) || p.host) === key) ?? null;
  const credential = profile?.wake_credential ?? "";
  const registKey = profile?.wake_regist_key ?? "";
  const rpKey = profile?.wake_rp_key ?? "";
  const hasSessionKeys = !!registKey && !!rpKey;
  return {
    profile,
    credential,
    registKey,
    rpKey,
    hasSessionKeys,
    /** Session keys imply a wake credential (it derives from the regist key). */
    canWake: !!credential || hasSessionKeys,
    canSignIn: hasSessionKeys,
  };
}

/**
 * Wake one console: "Wake & sign in" when its session keys are stored, a plain wake with its
 * credential otherwise. Shared by the power card and the rest-mode notice so the two never
 * drift (and both read THAT console's profile, not whichever one is selected). When the PS5
 * user has a login passcode, Wake & sign in asks for it (wakeSignIn.ts, WakePasscodeHost).
 */
export function useConsoleWake(host: string) {
  const tr = useTr();
  const profiles = useRosterStore((st) => st.profiles);
  const creds = wakeCredentialsFor(profiles, host);
  const [plainBusy, setBusy] = useState(false);
  const [plainError, setError] = useState<string | null>(null);
  // Wake & sign in runs in a store: it outlives this screen while the passcode box is up.
  const signInBusy = useWakeSignInStore((s) => !!s.busy[host]);
  const signInError = useWakeSignInStore((s) => s.error[host] ?? null);
  const busy = plainBusy || signInBusy;
  const error = creds.canSignIn ? signInError : plainError;

  async function wake() {
    if (!host || !creds.canWake || busy) return;
    if (creds.canSignIn) {
      // Wake AND sign the user in: it waits for the console to boot, then establishes the
      // control session (and asks for the passcode if the PS5 wants it).
      await wakeAndSignInAsking(host, creds);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await powerWake(host, creds.credential);
      // Not "waking up": the console never acknowledges, and ignores the request entirely
      // unless Remote Play is enabled.
      pushNotification(
        "info",
        tr("power_wake_sent", undefined, "Wake signal sent"),
        {
          body: tr("power_wake_sent_body", undefined,
            "The console doesn't confirm a wake. If it stays asleep, check that Remote Play, Stay Connected to the Internet, and Turning On PS5 from Network are all enabled on the console."),
        },
      );
      audit("system_wake", withConsolePrefix(host, "wake request sent"));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return { ...creds, busy, error, wake };
}
