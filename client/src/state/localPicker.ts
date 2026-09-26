// Global in-app file/folder picker, used on Android where the native
// dialog can't return a real filesystem path (scoped storage gives
// content:// URIs / no folder paths). One <LocalPathPicker/> is mounted
// at the app root; any screen requests a path imperatively:
//
//   import { pickLocalPath } from "../../state/localPicker";
//   const path = await pickLocalPath({ mode: "folder" });
//
// Desktop screens keep using @tauri-apps/plugin-dialog (real paths); they
// branch on isAndroid() before calling this. Every platform uses it to browse
// a saved server (`source: { connectionId }`).

import { create } from "zustand";

export interface LocalPickOptions {
  mode: "file" | "folder";
  /** Optional modal title override. */
  title?: string;
  /** Only show files with these extensions (folders are always shown). */
  filters?: { name: string; extensions: string[] }[];
  /** A saved server to browse instead of this device. Resolves with a `remote://` path. */
  source?: "local" | { connectionId: string };
  /** Opened to look around (Connections → Browse): each file row gets Install / Send to PS5. */
  actions?: { onInstall: (path: string) => void; onSend: (path: string) => void };
}

interface PendingReq extends LocalPickOptions {
  resolve: (path: string | null) => void;
}

interface LocalPickerState {
  pending: PendingReq | null;
  /** Open the picker; resolves with the chosen real path, or null. */
  open: (opts: LocalPickOptions) => Promise<string | null>;
  /** Close, resolving the in-flight request. */
  settle: (path: string | null) => void;
}

export const useLocalPickerStore = create<LocalPickerState>((set, get) => ({
  pending: null,
  open: (opts) =>
    new Promise<string | null>((resolve) => {
      // Only one picker at a time — cancel any already-open request.
      const prev = get().pending;
      if (prev) prev.resolve(null);
      set({ pending: { ...opts, resolve } });
    }),
  settle: (path) => {
    const req = get().pending;
    if (req) {
      req.resolve(path);
      set({ pending: null });
    }
  },
}));

/** Imperative helper for screens. */
export const pickLocalPath = (opts: LocalPickOptions) =>
  useLocalPickerStore.getState().open(opts);
