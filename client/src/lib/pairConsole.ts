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
export async function pairConsole(host: string, profileId: string, addr = mgmtAddr(host)): Promise<string | null> {
  const r = await pairForWake(host, addr);
  const st = useRosterStore.getState();
  st.setWakeCredential(profileId, r.credential);
  // Both keys arrive from the same handshake, already padded to the 16-byte form the session
  // code parses.
  if (isValidSessionKey(r.regist_key) && isValidSessionKey(r.rp_key)) {
    st.setWakeSessionKeys(profileId, r.regist_key, r.rp_key);
  }
  audit("system_wake", withConsolePrefix(host, "paired for wake"));
  return r.nickname || null;
}
