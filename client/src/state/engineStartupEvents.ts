import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useConnectionStore } from "./connection";
import { setLiveEngineUrl } from "./engine";
import { log } from "./logs";

let installed = false;

export function installEngineStartupEvents(): void {
  if (installed) return;
  installed = true;

  // Follow the sidecar to wherever it actually bound. Normally that's the
  // default 127.0.0.1:19113, but the Rust shell falls back to a free port
  // when 19113 is occupied (e.g. a standalone ps5upload-engine the user
  // launched by mistake) — and the renderer's DIRECT fetches (job polling,
  // cover-art img-src, streaming) must hit the real port. Two paths, so we
  // catch it regardless of ordering: the event (fired when start()
  // finishes) AND a one-shot pull (covers the event firing before this
  // listener attached, when the engine came up faster than the UI).
  void listen<string>("ps5upload-engine-ready", (event) => {
    if (typeof event.payload === "string" && event.payload) {
      setLiveEngineUrl(event.payload);
    }
  }).catch(() => {
    /* best-effort subscription */
  });
  void invoke<string>("engine_url_get")
    .then((url) => {
      if (typeof url === "string" && url) setLiveEngineUrl(url);
    })
    .catch(() => {
      /* command unavailable (mobile / older shell) — the event path
         and the default URL still apply */
    });
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
