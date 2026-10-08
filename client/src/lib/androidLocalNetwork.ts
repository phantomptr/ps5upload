/**
 * Android 17's local network permission.
 *
 * An app built for Android 17 (targetSdk 37) cannot connect to devices on the
 * local network until the user allows ACCESS_LOCAL_NETWORK. Without it every
 * connection to the PS5 fails, which the app reported as "Port 9021 is not
 * open" while the console was reachable. The Android shell (MainActivity,
 * patched by scripts/release/android-postinit-patch.sh) asks at launch and
 * exposes `window.PS5UploadNet` so the page can check and ask again.
 *
 * Everywhere the bridge is absent (desktop, browser build, iOS, Android below
 * 17 where it always reports granted) nothing is blocked.
 */

interface LocalNetworkBridge {
  granted(): boolean;
  request(): void;
}

type WindowWithNet = Window & { PS5UploadNet?: LocalNetworkBridge };

function bridge(w: Window | undefined): LocalNetworkBridge | undefined {
  return w ? (w as WindowWithNet).PS5UploadNet : undefined;
}

/** True only when Android says the app may not reach the local network. */
export function localNetworkBlocked(
  w: Window | undefined = typeof window !== "undefined" ? window : undefined,
): boolean {
  const b = bridge(w);
  if (!b) return false;
  try {
    return !b.granted();
  } catch {
    // A bridge that throws is treated like one that is absent.
    return false;
  }
}

/** Show Android's prompt, or the app's settings once Android stops showing it. */
export function requestLocalNetwork(
  w: Window | undefined = typeof window !== "undefined" ? window : undefined,
): void {
  try {
    bridge(w)?.request();
  } catch {
    // Nothing to do: the banner stays up and the user can try again.
  }
}
