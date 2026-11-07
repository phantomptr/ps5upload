// Desktop drag-and-drop of a path onto a screen, through the webview's drop events (Tauri
// delivers host paths). Not on Android or in the browser build, where pickers cover it.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useRef, useState } from "react";

import { isMobile } from "./platform";
import { isTauriEnv } from "./tauriEnv";
import { engineIsOnThisDevice } from "../state/engine";

/**
 * Every dropped path, packages included, while `enabled` (the Files screen copies whatever is
 * dropped into the open folder; Workshop converts a dropped .pkg like any other game). Returns
 * whether a drag is over the window. A screen that takes drops is listed in packageViewer's
 * OWN_DROPS, so the app-wide listener leaves them to it: a filter here that dropped packages
 * left a .pkg dropped on Workshop with nobody to take it (#716).
 */
export function useWebviewDropAll(onDrop: (paths: string[]) => void, enabled: boolean): boolean {
  return useWebviewDropCore((paths) => {
    if (paths.length > 0) onDrop(paths);
  }, enabled);
}

function useWebviewDropCore(onDrop: (paths: string[]) => void, enabled: boolean): boolean {
  const [active, setActive] = useState(false);
  const latest = useRef({ onDrop, enabled });
  // The subscription outlives renders; it reads the newest handler through this ref.
  useEffect(() => {
    latest.current = { onDrop, enabled };
  });

  useEffect(() => {
    // A dropped path is on this device; a remote engine can't open it.
    if (!isTauriEnv() || isMobile() || !engineIsOnThisDevice()) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (cancelled) return;
      const t = e.payload.type;
      if (t === "enter" || t === "over") setActive(latest.current.enabled);
      else if (t === "leave") setActive(false);
      else if (t === "drop") {
        setActive(false);
        if (latest.current.enabled) latest.current.onDrop(e.payload.paths ?? []);
      }
    });
    p.then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    }).catch(() => {
      // The webview went away before the subscription completed: nothing to undo.
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return active;
}
