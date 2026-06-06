import { listen } from "@tauri-apps/api/event";
import { useConnectionStore } from "./connection";
import { log } from "./logs";

let installed = false;

export function installEngineStartupEvents(): void {
  if (installed) return;
  installed = true;
  void listen<string>("ps5upload-engine-startup-error", (event) => {
    const message =
      typeof event.payload === "string"
        ? event.payload
        : "engine failed to start";
    useConnectionStore.getState().setStatus({
      engineStatus: "down",
      engineError: message,
    });
    log.error("engine", message);
  }).catch((e) => {
    log.warn(
      "engine",
      "could not subscribe to engine startup diagnostics",
      e,
    );
  });

  // Post-startup engine death (the sidecar crashed/exited mid-session). The
  // Tauri side detects it via the engine's stderr hitting EOF and emits this.
  // Previously a mid-session engine crash was invisible — it just looked like
  // API calls hanging/timing out.
  void listen<number | null>("ps5upload-engine-exit", (event) => {
    const code = event.payload;
    useConnectionStore.getState().setStatus({
      engineStatus: "down",
      engineError: `engine exited (code ${code ?? "unknown"})`,
    });
    log.error(
      "engine",
      `engine process exited unexpectedly (code ${code ?? "unknown"})`,
    );
  }).catch(() => {
    /* best-effort subscription */
  });
}
