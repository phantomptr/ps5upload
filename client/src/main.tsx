import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router";
import App from "./App";
import { RootErrorBoundary } from "./components";
// Import the theme + lang stores before the first render so their
// module-load side effects (setting <html data-theme> + <html lang dir>)
// complete before React paints.
import "./state/theme";
import "./state/uiScale";
import "./state/accessibility";
import "./lib/androidInsets";
import "./state/lang";
import "./index.css";
import {
  installUserConfigMirror,
  hydrateFromUserConfig,
} from "./state/userConfig";
import { installConsoleCapture, installDiskLogSink, log } from "./state/logs";
import { installEngineLogBridge } from "./state/engineLogBridge";
import { installEngineStartupEvents } from "./state/engineStartupEvents";
import { installAccidentalReloadGuard } from "./lib/preventAccidentalReload";

// Capture console.error / warn + unhandled promise rejections into
// the in-app log store before any other module runs — that way the
// Log tab shows *everything* that happens after app boot, including
// bugs in our own initialization code.
installConsoleCapture();
// Persist the unified log (frontend + engine bridge + console + payload
// events) to ~/.ps5upload/logs/ so a bug report can package a time window
// even after a crash. Best-effort; no-ops outside Tauri. See state/logs.ts.
installDiskLogSink();
log.info("app", "ps5upload client booting");
installEngineStartupEvents();

// Auto-collect a detailed diagnostic report to ~/.ps5upload/crash-reports/
// whenever the app surfaces an error (error-level notification) or the React
// tree crashes. Wired right after console capture so the very first errors
// are covered. Best-effort + debounced; see lib/crashReporter.ts.
import("./lib/crashReporter").then((m) => m.initCrashReporter());

// Mirror engine sidecar log lines into the Log tab. Without this, the
// engine's diagnostic output (reconcile progress, transfer retries,
// command errors) only reaches the dev terminal during `tauri dev` —
// packaged users would see nothing when something goes wrong.
installEngineLogBridge();

// Start mirroring stores to ~/.ps5upload/settings.json on every change,
// and then hydrate from that file (asynchronously) if the user has a
// pre-existing or hand-edited copy. Mirror runs regardless of whether
// hydration finds a file — that way a fresh install starts writing
// immediately.
installUserConfigMirror();
// Fire-and-forget: if the Tauri command isn't registered yet (dev mode
// first launch before compile), the inner call silently no-ops.
void hydrateFromUserConfig();

// Suppress the WebView's right-click menu (Back/Reload/Inspect) so a stray
// click can't restart the app out from under a running transfer/install;
// reload keyboard shortcuts are additionally blocked in production. No-op on
// non-Tauri / Android. See preventAccidentalReload.ts for the dev/prod split.
installAccidentalReloadGuard();

// Android soft-keyboard avoidance: when the keyboard appears, the visualViewport
// shrinks. Scroll the focused input into view so it isn't hidden behind the
// keyboard. Uses visualViewport API (available on Android Chrome/WebView 61+).
if (typeof window !== "undefined" && window.visualViewport) {
  const onResize = () => {
    const active = document.activeElement;
    if (
      active &&
      (active.tagName === "INPUT" ||
        active.tagName === "TEXTAREA" ||
        (active as HTMLElement).isContentEditable)
    ) {
      const el = active as HTMLElement;
      const vv = window.visualViewport!;
      const rect = el.getBoundingClientRect();
      // If the input's bottom is below the visual viewport bottom, scroll it into view
      if (rect.bottom > vv.height + vv.offsetTop) {
        el.scrollIntoView({ block: "center", behavior: "smooth" });
      }
    }
  };
  window.visualViewport.addEventListener("resize", onResize);
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <RootErrorBoundary>
      {/* react-router 8 removed the react-router-dom package;
          imports come from react-router and react-router/dom. */}
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </RootErrorBoundary>
  </React.StrictMode>
);
