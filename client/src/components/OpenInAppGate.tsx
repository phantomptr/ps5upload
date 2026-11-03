// "Open in the PS5Upload app?" before a browser page (the Play Together invite, the phone
// controller / watch link) starts, the way Slack or Zoom links ask: open it in the app, where it
// is installed, or carry on in the browser. The choice can be remembered on this device; a
// remembered "app" opens the app at once and still offers the browser if nothing happened.
// Built into the engine's /join and /pad pages (no app shell there), so plain elements only.

import { useEffect, useState, type ReactNode } from "react";

import { safeGetItem, safeRemoveItem, safeSetItem } from "../lib/safeStorage";
import { useTr } from "../state/lang";

/** Where this device's remembered choice lives ("app" or "browser"). */
export const OPEN_IN_KEY = "ps5upload.openIn";
/** This tab already carried on in the browser: a reload (a controller mid-game) doesn't ask again. */
const TAB_KEY = "ps5upload.openIn.tab";

function tabInBrowser(): boolean {
  try {
    return sessionStorage.getItem(TAB_KEY) === "browser";
  } catch {
    return false;
  }
}

function markTabInBrowser(): void {
  try {
    sessionStorage.setItem(TAB_KEY, "browser");
  } catch {
    // No storage: a reload asks again.
  }
}

type Phase = "ask" | "opening" | "browser";

/** Where a page starts: the remembered choice, else the question. */
export function startPhase(saved: string | null): Phase {
  return saved === "browser" ? "browser" : saved === "app" ? "opening" : "ask";
}

export function OpenInAppGate({ appLink, children }: { appLink: string; children: ReactNode }) {
  const tr = useTr();
  const [phase, setPhase] = useState<Phase>(() =>
    tabInBrowser() ? "browser" : startPhase(safeGetItem(OPEN_IN_KEY)),
  );
  const [remember, setRemember] = useState(false);

  // A remembered "app": straight to it, once.
  useEffect(() => {
    if (phase === "opening" && safeGetItem(OPEN_IN_KEY) === "app") location.href = appLink;
    // Once, on load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (phase === "browser") return <>{children}</>;

  const toApp = () => {
    if (remember) safeSetItem(OPEN_IN_KEY, "app");
    setPhase("opening");
    location.href = appLink;
  };
  const toBrowser = () => {
    // Asked: remember "browser" when ticked. After "Opening…" (it didn't open): forget a
    // remembered "app", so the next link asks again.
    if (phase === "ask" && remember) safeSetItem(OPEN_IN_KEY, "browser");
    else if (phase === "opening" && safeGetItem(OPEN_IN_KEY) === "app") safeRemoveItem(OPEN_IN_KEY);
    markTabInBrowser();
    setPhase("browser");
  };

  return (
    <div className="flex min-h-[100dvh] items-center justify-center bg-black px-6 text-white" data-testid="open-in-gate">
      <div className="w-full max-w-sm space-y-4 rounded-2xl border border-white/10 bg-neutral-900 p-6">
        {phase === "ask" ? (
          <>
            <div className="space-y-1">
              <h1 className="text-lg font-semibold">{tr("open_in_title", undefined, "Open in the PS5Upload app?")}</h1>
              <p className="text-sm text-white/60">
                {tr("open_in_body", undefined, "Have the PS5Upload app on this device? Open this there, or carry on in the browser.")}
              </p>
            </div>
            <button
              type="button"
              onClick={toApp}
              className="w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black"
              data-testid="open-in-app"
            >
              {tr("open_in_app", undefined, "Open in the app")}
            </button>
            <button
              type="button"
              onClick={toBrowser}
              className="w-full rounded-full border border-white/20 py-2.5 text-sm font-semibold text-white hover:bg-white/10"
              data-testid="open-in-browser"
            >
              {tr("open_in_browser", undefined, "Continue in browser")}
            </button>
            <label className="flex items-center justify-center gap-2 text-xs text-white/60">
              <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} data-testid="open-in-remember" />
              {tr("open_in_remember", undefined, "Remember my choice on this device")}
            </label>
          </>
        ) : (
          <>
            <div className="space-y-1">
              <h1 className="text-lg font-semibold">{tr("open_in_opening", undefined, "Opening the PS5Upload app…")}</h1>
              <p className="text-sm text-white/60">
                {tr("open_in_not_opened", undefined, "Nothing happened? The app may not be on this device.")}
              </p>
            </div>
            <button
              type="button"
              onClick={() => (location.href = appLink)}
              className="w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black"
              data-testid="open-in-again"
            >
              {tr("open_in_again", undefined, "Try again")}
            </button>
            <button
              type="button"
              onClick={toBrowser}
              className="w-full rounded-full border border-white/20 py-2.5 text-sm font-semibold text-white hover:bg-white/10"
              data-testid="open-in-browser"
            >
              {tr("open_in_browser", undefined, "Continue in browser")}
            </button>
          </>
        )}
      </div>
    </div>
  );
}
