import type { ProcessInfo } from "../../api/ps5";

/** Which question a Kill asks before it runs, or null when the process cannot be killed
 *  from here (the helper itself). Every other kill asks: nothing on the console is safe to
 *  kill on one click, payloads included (kstuff going away stops fake-package games). */
export function killPrompt(p: ProcessInfo): ProcessInfo["kind"] | null {
  if (p.is_self) return null;
  return p.kind;
}
