// Desktop drag-and-drop of a path onto a screen, through the webview's drop events (Tauri
// delivers host paths). Not on Android or in the browser build, where pickers cover it.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useRef, useState } from "react";

import { isAndroid } from "./platform";
import { isTauriEnv } from "./tauriEnv";

/** A .pkg is a package to install; AppShell's app-wide drop listener routes those. */
const isPackage = (path: string) => /\.pkg$/i.test(path);

/**
 * Calls `onDrop` with the first dropped path while `enabled`; returns whether a drag is over
 * the window, for highlighting a drop zone. The subscription is cleaned up even when it
 * resolves after unmount, as Upload's is.
 */
export function useWebviewDrop(onDrop: (path: string) => void, enabled: boolean): boolean {
  const [active, setActive] = useState(false);
  const latest = useRef({ onDrop, enabled });
  // The subscription outlives renders; it reads the newest handler through this ref.
  useEffect(() => {
    latest.current = { onDrop, enabled };
  });

  useEffect(() => {
    if (!isTauriEnv() || isAndroid()) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (cancelled) return;
      const t = e.payload.type;
      if (t === "enter" || t === "over") setActive(latest.current.enabled);
      else if (t === "leave") setActive(false);
      else if (t === "drop") {
        setActive(false);
        const first = e.payload.paths?.[0];
        if (first && latest.current.enabled && !isPackage(first)) latest.current.onDrop(first);
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
