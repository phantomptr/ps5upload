/**
 * Session memory of whether this renderer can load images by URL at all.
 *
 * Cover art is the only thing in this app that an `<img>` loads directly
 * over HTTP; everything else goes through the Tauri IPC. Some webviews
 * refuse that load outright — a CSP or mixed-content decision made by
 * whatever WebKit ships with the user's OS — while the engine logs a
 * healthy 200. `useImageRetry` recovers by re-fetching the same bytes over
 * the IPC as a `data:` URL.
 *
 * The problem that motivates this module is that the recovery was being
 * rediscovered per image, per mount. Every cover began with a direct load,
 * failed, waited out a retry stagger of 1.2-2.4s, failed again, and only
 * then reached the transport that works. A grid of forty covers paid that
 * eighty times, and paid it again on the next visit to the screen — which
 * is what "the covers reload every time" actually was. The disk and memory
 * caches were never the problem; they simply never got a chance to be
 * fast, because nothing consulted them until two failures had elapsed.
 *
 * A blocked webview is a property of the environment, not of any one
 * image, so it only needs to be learned once. After that, callers with an
 * IPC fallback skip the direct attempt entirely.
 *
 * Deliberately session-scoped and in-memory. The decision depends on the
 * OS webview the app happens to be running in, so persisting it across
 * launches would mean carrying a verdict about an environment that may no
 * longer apply — and re-learning it costs one image, once.
 */

/** True once a direct load has failed for an image whose bytes the IPC
 *  fallback then produced successfully. */
let blocked = false;

/**
 * Record that the direct transport failed for an image the IPC could
 * fetch. Both halves matter: a title with genuinely no artwork also fails
 * its direct load, and latching on that alone would wrongly condemn a
 * working transport. Only "this image exists and only the IPC could get
 * it" is evidence about the transport itself.
 */
export function noteDirectTransportBlocked(): void {
  blocked = true;
}

/** Whether to skip the direct attempt and go straight to the IPC. */
export function isDirectTransportBlocked(): boolean {
  return blocked;
}

/** Test-only: forget what this session learned. */
export function resetImageTransport(): void {
  blocked = false;
}
