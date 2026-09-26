import { isTauriEnv } from "./tauriEnv";
import { getEngineUrl } from "../state/engine";

/**
 * Resolve the running app's version.
 *
 * Desktop/Tauri: the app package version (`getVersion()`), which is what
 * "outdated payload" / About-screen checks care about.
 *
 * Browser: there is no separate shell — the engine binary IS the app, so
 * its own `GET /api/version` is the equivalent. Unwraps the `{version}`
 * envelope so every call site keeps consuming a plain string either way.
 */
export async function getAppVersion(): Promise<string> {
  if (!isTauriEnv()) {
    const r = await fetch(`${getEngineUrl()}/api/version`);
    if (!r.ok) throw new Error(`engine HTTP ${r.status}`);
    const j = (await r.json()) as { version: string };
    return j.version;
  }
  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}
