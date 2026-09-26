import { create } from "zustand";

/**
 * Cross-screen "open this directory in the File System browser" request.
 *
 * Other screens (Saves, Install Package, Installed Apps…) call `requestPath`
 * then navigate to `/file-system`; the File System screen consumes the request
 * on mount/host-change and jumps there. A one-shot value (consumed once) so a
 * later manual navigation back to File System doesn't re-jump.
 */
interface FsNavState {
  requestedPath: string | null;
  /** Ask the File System screen to open `path` next time it mounts. */
  requestPath: (path: string) => void;
  /** Read-and-clear the pending request (returns null when none). */
  consume: () => string | null;
}

export const useFsNavStore = create<FsNavState>((set, get) => ({
  requestedPath: null,
  requestPath: (path) => set({ requestedPath: path }),
  consume: () => {
    const p = get().requestedPath;
    if (p != null) set({ requestedPath: null });
    return p;
  },
}));

/** Convenience: stash a directory request and route to the File System screen.
 *  `navigate` is the react-router navigate fn from the calling component. */
export function openInFileSystem(
  navigate: (to: string) => void,
  path: string,
): void {
  useFsNavStore.getState().requestPath(path);
  navigate("/files");
}
