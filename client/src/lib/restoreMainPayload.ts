import { invoke } from "./invokeLogged";
import { isTauriEnv } from "./tauriEnv";
import { log } from "../state/logs";

/**
 * Put the ps5upload helper back on a console after something displaced it.
 *
 * Environment-dependent by necessity, and getting it wrong is invisible until
 * a console is left with no helper at all:
 *
 *  - **Desktop** holds the ELF bytes and can open a socket to the loader, so
 *    it resolves the bundled path and sends it itself.
 *  - **Browser** can do neither. `payload_bundled_path` / `payload_send` are
 *    desktop-only commands; calling them from the web UI throws
 *    BrowserUnsupportedError, which is what made the whole DPI fallback — and
 *    therefore every patch install — fail from the web UI (#152). The engine
 *    owns the bytes and the socket there, via `payload_restore`.
 *
 * Extracted from pkgLibrary so every caller that can displace the helper gets
 * the same environment-aware restore. Never throws: a failed restore is logged
 * (it explains a helper that stayed offline in the next bug bundle) and the
 * app's reconnect watcher makes further attempts.
 */
export async function restoreMainPayload(ip: string): Promise<void> {
  try {
    if (!isTauriEnv()) {
      const r = (await invoke("payload_restore", { ip })) as {
        ok?: boolean;
        error?: string;
      };
      if (!r?.ok) {
        log.warn(
          "install",
          `couldn't restore the main payload on ${ip}: ${
            r?.error ?? "engine reported no payload image"
          }`,
        );
      }
      return;
    }
    const bp = (await invoke("payload_bundled_path")) as {
      ok?: boolean;
      path?: string;
    };
    if (bp?.ok && bp.path) {
      await invoke("payload_send", { ip, path: bp.path, port: null });
    }
  } catch (e) {
    log.warn(
      "install",
      `couldn't restore the main payload on ${ip}: ${
        e instanceof Error ? e.message : String(e)
      }`,
    );
  }
}
