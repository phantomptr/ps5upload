import { useCallback, useEffect, useRef, useState } from "react";
import { Tv } from "lucide-react";

import {
  changeConsolePrefs,
  cleanConsolePath,
  fetchConsolePrefs,
  handoffToConsole,
  type ConsoleFilesPrefs,
} from "../../api/consoleFiles";
import type { Translator } from "../../state/lang";
import type { ToastOptions } from "../../state/toasts";
import type { RowMenuItem } from "./RowActions";
import { humanizeCaught } from "../../lib/humanizeError";

export interface ConsolePins {
  /** The console's pinned folders; null when the helper has none to share (older, or not up). */
  pins: string[] | null;
  isPinned: (path: string) => boolean;
  /** Pin `path` on the console, or unpin it when it is pinned. Rejects with the engine's error. */
  toggle: (path: string) => Promise<void>;
  /** Read the console's list again (the TV may have changed it). */
  refresh: () => void;
  /** "Show on console": the console page opens `path`. Rejects with the engine's error. */
  showOnConsole: (path: string) => Promise<void>;
}

/** The folders pinned on the console's own Files page, shared with the File System screen. */
export function useConsolePins(host: string, up: boolean): ConsolePins {
  const [prefs, setPrefs] = useState<ConsoleFilesPrefs | null>(null);
  const [tick, setTick] = useState(0);
  /** The host the shown prefs came from: an answer for another console is dropped. */
  const hostRef = useRef(host);
  useEffect(() => {
    hostRef.current = host;
  }, [host]);

  useEffect(() => {
    if (!up || !host.trim()) {
      setPrefs(null);
      return;
    }
    let live = true;
    fetchConsolePrefs(host).then(
      (p) => live && setPrefs(p),
      () => live && setPrefs(null),
    );
    return () => {
      live = false;
    };
  }, [host, up, tick]);

  const isPinned = useCallback(
    (path: string) => !!prefs && prefs.favorites.includes(cleanConsolePath(path)),
    [prefs],
  );

  const toggle = useCallback(
    async (path: string) => {
      const asked = host;
      const p = await changeConsolePrefs(host, isPinned(path) ? "unpin" : "pin", cleanConsolePath(path));
      if (hostRef.current === asked) setPrefs(p);
    },
    [host, isPinned],
  );

  const refresh = useCallback(() => setTick((n) => n + 1), []);

  const showOnConsole = useCallback(
    async (path: string) => {
      if (!(await handoffToConsole(host, cleanConsolePath(path)))) {
        setPrefs(null); // an older helper after all: hide the features
        throw new Error("unsupported");
      }
    },
    [host],
  );

  return { pins: prefs ? prefs.favorites : null, isPinned, toggle, refresh, showOnConsole };
}

/** The context menu's "Show on console" for `path` (none against an older helper). */
export function showOnConsoleItems(
  pins: ConsolePins,
  path: string,
  tr: Translator,
  toast: (t: ToastOptions) => unknown,
): RowMenuItem[] {
  if (pins.pins == null) return [];
  return [
    {
      icon: Tv,
      label: tr("fs_show_on_console", undefined, "Show on console"),
      run: () =>
        void pins.showOnConsole(path).then(
          () =>
            toast({
              tone: "success",
              message: tr(
                "fs_show_on_console_sent",
                undefined,
                "Sent to the PS5 — it opens in PS5Upload Minion",
              ),
            }),
          (e: unknown) =>
            toast({
              tone: "warn",
              message: tr(
                "fs_show_on_console_failed",
                { error: humanizeCaught(e) },
                `Couldn't send it to the PS5: ${e instanceof Error ? e.message : String(e)}`,
              ),
            }),
        ),
    },
  ];
}
