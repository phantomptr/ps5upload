import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
// Import the theme + lang stores before the first render so their
// module-load side effects (setting <html data-theme> + <html lang dir>)
// complete before React paints.
import "./state/theme";
import "./state/lang";
import "./index.css";
import {
  installUserConfigMirror,
  hydrateFromUserConfig,
} from "./state/userConfig";
import { installConsoleCapture, log } from "./state/logs";
import { installEngineLogBridge } from "./state/engineLogBridge";

// Capture console.error / warn + unhandled promise rejections into
// the in-app log store before any other module runs — that way the
// Log tab shows *everything* that happens after app boot, including
// bugs in our own initialization code.
installConsoleCapture();
log.info("app", "ps5upload client booting");

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

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <BrowserRouter
      future={{
        v7_startTransition: true,
        v7_relativeSplatPath: true,
      }}
    >
      <App />
    </BrowserRouter>
  </React.StrictMode>
);
