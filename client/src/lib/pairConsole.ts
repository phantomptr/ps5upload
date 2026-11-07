// Pair this app with a console (Remote Play registration, run on the console by the helper):
// the wake credential and both session keys come back from one call and are kept with the
// console's saved profile. Connection's Wake & sign in and Play's setup checklist both use it.

import { pairForWake } from "../api/ps5";
import { audit } from "../state/auditLog";
import { useRosterStore, withConsolePrefix } from "../state/roster";
import { mgmtAddr } from "./addr";
import { isValidSessionKey } from "./wakeState";

/** Pair and keep the keys. Returns the console's nickname when it said one. Throws the engine's
 *  own error (already specific: "no signed-in user with an activated PSN account…"). */
export async function pairConsole(
  host: string,
  profileId: string,
  addr = mgmtAddr(host),
  /** The PIN the PS5 shows under Link Device, and the account to pair (no helper needed). */
  manual?: { pin: string; accountIdB64: string; userName?: string },
): Promise<string | null> {
  const r = await pairForWake(host, addr, manual);
  const st = useRosterStore.getState();
  // Both keys arrive from the same handshake, already padded to the 16-byte form the session
  // code parses. A pairing is one PS5 user's: it joins the console's paired users (another
  // user's pairing stays) and becomes the one Start and Wake use.
  if (isValidSessionKey(r.regist_key) && isValidSessionKey(r.rp_key)) {
    st.addRpPairing(profileId, {
      account_id: r.account_id_b64 ?? "",
      name: r.user_name || manual?.userName || "",
      wake_credential: r.credential,
      wake_regist_key: r.regist_key,
      wake_rp_key: r.rp_key,
    });
  } else {
    st.setWakeCredential(profileId, r.credential);
  }
  audit("system_wake", withConsolePrefix(host, manual ? "paired with the PS5's PIN" : "paired for wake"));
  return r.nickname || null;
}
