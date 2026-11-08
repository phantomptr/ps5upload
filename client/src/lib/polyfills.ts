// Small stand-ins for web APIs an older WebKit lacks, installed before anything else runs.
//
// The macOS app runs on the system's WebKit, so a Mac on macOS 11 or 12 without a newer Safari
// has Safari 14 or 15's engine (tauri.conf: minimumSystemVersion 11.0), and the PS5's own browser
// and older phones are similar. Without these, every call that used them failed with
// "AbortSignal.timeout is not a function" (#765): moving and deleting files, Settings, and more.
//
//   AbortSignal.timeout(ms)   Safari 16.0
//   crypto.randomUUID()       Safari 15.4
//
// Each is installed only when missing, so a current browser keeps its own.

/** The id `crypto.randomUUID()` gives (version 4), from `crypto.getRandomValues`. */
export function uuidV4(fill: (b: Uint8Array<ArrayBuffer>) => void): string {
  const b = new Uint8Array(16);
  fill(b);
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** An AbortSignal that aborts after `ms` with a TimeoutError, as `AbortSignal.timeout` does. */
export function timeoutSignal(ms: number): AbortSignal {
  const c = new AbortController();
  setTimeout(() => {
    let reason: unknown;
    try {
      reason = new DOMException("The operation timed out.", "TimeoutError");
    } catch {
      reason = Object.assign(new Error("The operation timed out."), { name: "TimeoutError" });
    }
    // An older abort() takes no reason: the signal still aborts, which is what callers wait for.
    (c.abort as (r?: unknown) => void)(reason);
  }, ms);
  return c.signal;
}

/** Installs what is missing. Safe to call more than once. */
export function installPolyfills(g: typeof globalThis = globalThis): void {
  const signal = g.AbortSignal as (typeof AbortSignal & { timeout?: unknown }) | undefined;
  if (signal && typeof signal.timeout !== "function") {
    Object.defineProperty(signal, "timeout", { value: timeoutSignal, configurable: true, writable: true });
  }
  const c = g.crypto as (Crypto & { randomUUID?: unknown }) | undefined;
  if (c && typeof c.randomUUID !== "function" && typeof c.getRandomValues === "function") {
    Object.defineProperty(c, "randomUUID", {
      value: () => uuidV4((b) => void c.getRandomValues(b)),
      configurable: true,
      writable: true,
    });
  }
}

installPolyfills();
