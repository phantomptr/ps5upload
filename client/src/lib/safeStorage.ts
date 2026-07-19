/**
 * Safe localStorage wrappers. `localStorage.setItem` throws in several
 * common scenarios:
 *
 *  - Safari Private Mode: quota is 0, every setItem throws QuotaExceededError.
 *  - Storage quota exceeded: large JSON caches (pkg library, activity history,
 *    notifications) can exhaust the ~5 MB origin budget.
 *  - Disabled by browser settings / enterprise policy (rare but seen in
 *    locked-down WebView2 / WebKit deployments).
 *
 * The app uses localStorage purely as a best-effort persistence layer —
 * every value has a built-in default or can be reconstructed from the
 * engine. So a failed write should never crash the UI; it should silently
 * degrade (the next read returns the default / stale value).
 *
 * These wrappers absorb the throw and log to console.warn for debugging.
 */

export function safeSetItem(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch (e) {
    console.warn(`localStorage.setItem failed for key "${key}":`, e);
  }
}

export function safeGetItem(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch (e) {
    console.warn(`localStorage.getItem failed for key "${key}":`, e);
    return null;
  }
}

export function safeRemoveItem(key: string): void {
  try {
    window.localStorage.removeItem(key);
  } catch (e) {
    console.warn(`localStorage.removeItem failed for key "${key}":`, e);
  }
}
