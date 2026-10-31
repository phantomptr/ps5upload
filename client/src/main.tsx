/**
 * Entry point. In the self-hosted web UI the settings every browser shares are read from the
 * engine first (lib/sharedPrefs), so each store reads them as its own when the app loads; then
 * the app starts (appMain). The desktop and mobile apps start at once.
 */
import { installNativePad } from "./lib/nativePad";
import { isTauriEnv } from "./lib/tauriEnv";
import { preloadSharedPrefs } from "./lib/sharedPrefs";

// The Android app forwards controllers from the first moment the page exists.
installNativePad();
// The Linux app reads controllers itself, for a WebKitGTK whose Gamepad API sees none.
if (isTauriEnv()) void import("./lib/nativePadDesktop").then((m) => m.startNativePads());

const ready = isTauriEnv() ? Promise.resolve() : preloadSharedPrefs();
void ready.then(() => import("./appMain"));
