/**
 * "Check the consoles now": asked by whatever learns something the status poller cannot see
 * coming (a wake was just sent, the network came back, the window got the focus back), heard by
 * the poller (AppShell's useStatusPolling), which then asks at once instead of at its next
 * round. With `wake`, that console is asked every few seconds for a minute while it comes up.
 */

export interface RecheckRequest {
  /** One console's address; absent: every console. */
  host?: string;
  /** A console that was just woken (or a network that just came back): ask often for a while. */
  wake?: boolean;
}

type Listener = (r: RecheckRequest) => void;
const listeners = new Set<Listener>();

export function requestStatusRecheck(r: RecheckRequest = {}): void {
  for (const fn of listeners) {
    try {
      fn(r);
    } catch (e) {
      console.warn("status recheck listener failed:", e);
    }
  }
}

export function onStatusRecheck(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
