import { processKill, type ProcessInfo, type ProcessKillAck } from "../../api/ps5";

/** Which question a Kill asks before it runs, or null when the process cannot be killed
 *  from here (the helper itself). Every other kill asks: nothing on the console is safe to
 *  kill on one click, payloads included (kstuff going away stops fake-package games). */
export function killPrompt(p: ProcessInfo): ProcessInfo["kind"] | null {
  if (p.is_self) return null;
  return p.kind;
}

/** Kill `pid`, throwing when the helper didn't: a refused or failed kill comes back as an
 *  answer with `ok: false`, not as an error. Running and On your PS5 both stop through this. */
export async function killChecked(
  addr: string,
  pid: number,
  kill: (addr: string, pid: number) => Promise<ProcessKillAck> = processKill,
): Promise<void> {
  const ack = await kill(addr, pid);
  if (!ack?.ok) throw new Error(ack?.err || `pid ${pid} was not stopped`);
}
