import { StrictMode, useEffect, useState, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import "virtual:pc-tokens.css";
import "./console.css";
import { App } from "./App";
import { call, probeHelper } from "./api";
import { INPUT_LOG_SWITCH, setInputLogEnabled } from "./inputLog";
import { installBackHistory } from "./backHistory";
import { installFullscreen } from "./fullscreen";
import { installMarquee } from "./ui/marquee";
import { DEFAULT_INPUT_MAP, dispatchNav, installNavigation } from "./nav";
import { installOfflineCopy } from "./offline/offlineCopy";
import { OfflinePage } from "./offline/OfflinePage";
import { UpdateBanner } from "./offline/UpdateBanner";
import { isNewerHelper, markUpdateReady } from "./offline/update";
import { ErrorBoundary } from "./ui/ErrorBoundary";
import { applyStoredTheme } from "./theme";
import { getLanguage, onLanguageChange, setLanguage, stringsVersion, t } from "./strings";

applyStoredTheme(); // before the first paint: no flash of the wrong theme
// English is in the page; the detected (or stored) language loads from the helper and the page
// re-renders in it when it arrives (<html lang/dir> follow).
setLanguage(getLanguage());
installNavigation(DEFAULT_INPUT_MAP);
// Full screen on the first press while the Settings switch is on (the default).
installFullscreen();
// The browser's Back (the PS5's ○) steps back inside the page: see backHistory.ts.
installBackHistory(() => dispatchNav("back"));
// Long names scroll inside their row while focused.
installMarquee();
// The offline copy (service worker + appcache): the tile opens the "not running" screen, not an error.
installOfflineCopy();

/**
 * Re-renders the whole page when its strings change (a language loaded or chosen in Settings).
 * First asks the helper for the token: no answer = this is the offline copy and the helper is not
 * running (OfflinePage). Any render error shows ErrorBoundary instead of a blank page.
 */
function Root() {
  // An external store, so a load that lands before the first commit is not missed.
  useSyncExternalStore(onLanguageChange, stringsVersion);
  const [up, setUp] = useState<boolean | null>(null);
  useEffect(() => {
    let live = true;
    void probeHelper().then((ok) => {
      if (!live) return;
      setUp(ok);
      // An older page than the helper (the browser's offline copy, or the helper updated
      // underneath): offer the one-press reload. An older helper's reply is just not compared.
      if (ok) {
        call<{ version?: unknown }>("/api/settings").then(
          (s) => {
            if (isNewerHelper(s?.version)) markUpdateReady();
          },
          () => undefined,
        );
        // A measuring session (INPUT_LOG_SWITCH on the console): log input on every screen.
        call<{ missing?: unknown }>("/api/fs/stat", { path: INPUT_LOG_SWITCH, missing_ok: true }).then(
          (st) => {
            if (st && st.missing !== true) setInputLogEnabled(true);
          },
          () => undefined,
        );
      }
    });
    return () => {
      live = false;
    };
  }, []);
  // A moment at most (the token read gives up after 4 s): the name, not a blank screen.
  if (up === null)
    return (
      <div className="cw-splash" aria-busy="true">
        {t("cw_settings_app_name")}
      </div>
    );
  return (
    <ErrorBoundary>
      {up ? <App inputMap={DEFAULT_INPUT_MAP} /> : <OfflinePage />}
      <UpdateBanner />
    </ErrorBoundary>
  );
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <Root />
    </StrictMode>,
  );
}
