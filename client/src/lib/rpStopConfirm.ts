// Stopping Play ends Play Together for everyone in it: every Stop asks first while friends are in.

import { friendsIn, joinApi } from "../screens/Play/playTogetherState";

/** Friends in the game on `host` now; 0 when that can't be read (a Stop must still work). */
export async function friendsPlaying(host: string | undefined | null): Promise<number> {
  if (!host) return 0;
  const s = await joinApi.read(host).catch(() => null);
  return s ? friendsIn(s).length : 0;
}
