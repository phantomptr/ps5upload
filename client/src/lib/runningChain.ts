// Which of the setup wizard's pre-helper payloads (kstuff, ShadowMount+) are
// already running on a console, read from its process list.
//
// The wizard used to send kstuff every time it ran. A console whose own
// autoloader had already loaded it ended up with two copies, and every
// "Run again" (including the retry after a slow helper boot) added another —
// one bug report showed eight. Skipping what is already there is what stops it.

export type ChainPayload = "kstuff" | "shadowmount";

/** The chain payloads present in `processes`. Matches on the thread and
 *  command names, case-insensitively: every kstuff build we have seen
 *  (kstuff.elf, kstuff-lite, Kstuff-NG) and shadowmountplus.elf. */
export function runningChainPayloads(
  processes: ReadonlyArray<{ name: string; comm?: string }>,
): Set<ChainPayload> {
  const found = new Set<ChainPayload>();
  for (const p of processes) {
    const names = `${p.name} ${p.comm ?? ""}`.toLowerCase();
    if (names.includes("kstuff")) found.add("kstuff");
    if (names.includes("shadowmount")) found.add("shadowmount");
  }
  return found;
}
