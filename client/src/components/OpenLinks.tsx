// ps5upload:// links opened with the app (a Play Together invite's "Open in the PS5Upload app").
// The shell keeps each link until the page takes it (open_links.rs): subscribe first, then take,
// so a link that opened the app before the page loaded is not lost. A join link goes to Play ›
// Join a friend with the invite filled in, which asks the host as usual; a controller (or watch)
// link to this device as that computer's controller (/play/pad).

import { useEffect } from "react";
import { useNavigate } from "react-router";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { isTauriEnv } from "../lib/tauriEnv";
import { appLinkRoute } from "../lib/appLinks";
import { noteAppLinks } from "../lib/appLinkDiag";

/** When the page asks the shell again for links while the app starts (ms after it loads). */
export const RETAKE_MS = [500, 1500, 3000, 6000];

/** Where an opened link leads in the app, or null for one it doesn't know (lib/appLinks). */
export const openLinkRoute = appLinkRoute;

export function OpenLinks(): null {
  const navigate = useNavigate();
  useEffect(() => {
    if (!isTauriEnv()) return;
    let alive = true;
    const drain = () => {
      invoke<string[]>("open_links_take")
        .then((links) => {
          if (!alive) return;
          noteAppLinks(links);
          // The newest one wins when several arrived at once.
          const route = links.map(openLinkRoute).filter((r): r is string => !!r).pop();
          if (route) navigate(route);
        })
        .catch(() => {
          /* an older shell without the command */
        });
    };
    const unlisten = listen("ps5upload-open-link", drain)
      .catch(() => null)
      .finally(drain);
    // A link that opened the app from closed can reach the shell after the page's first take
    // (on Android the launch intent arrives late, and its event before anyone listens): it then
    // sat there until the next link came. Ask again while the app starts, and whenever it comes
    // back to the front.
    const retries = RETAKE_MS.map((ms) => window.setTimeout(drain, ms));
    const onShow = () => {
      if (document.visibilityState === "visible") drain();
    };
    document.addEventListener("visibilitychange", onShow);
    window.addEventListener("focus", drain);
    return () => {
      alive = false;
      retries.forEach((t) => window.clearTimeout(t));
      document.removeEventListener("visibilitychange", onShow);
      window.removeEventListener("focus", drain);
      void unlisten.then((off) => off?.());
    };
  }, [navigate]);
  return null;
}
