// The app-wide package viewer: any screen (or a drop anywhere) opens it on a path; AppShell
// mounts the one panel. Actions come from whoever opened it — the viewer starts nothing itself.

import { create } from "zustand";

import type { PanelAction } from "../components/PackagePanel";

interface ViewerState {
  request: { path: string; actions: PanelAction[] } | null;
  open: (path: string, actions?: PanelAction[]) => void;
  close: () => void;
}

export const usePackageViewer = create<ViewerState>((set) => ({
  request: null,
  open: (path, actions = []) => set({ request: { path, actions } }),
  close: () => set({ request: null }),
}));

/** Screens with their own drop zone keep their drops. */
// Files copies whatever is dropped into the open folder, a .pkg included (no install offer).
const OWN_DROPS = ["/install-package", "/payloads", "/upload", "/convert", "/files"];

/** The first dropped thing the viewer can show — a package, a game image, or a folder (a path
 *  with no extension) — unless this screen takes drops itself. */
export function dropTarget(
  paths: string[],
  pathname: string,
): { path: string; kind: "package" | "game" } | null {
  if (OWN_DROPS.includes(pathname)) return null;
  for (const p of paths) {
    const name = p.split(/[\\/]/).pop() ?? p;
    if (/\.f?pkg$/i.test(name)) return { path: p, kind: "package" };
    if (/\.(exfat|ffpkg|ffpfsc)$/i.test(name) || !name.includes(".")) return { path: p, kind: "game" };
  }
  return null;
}
