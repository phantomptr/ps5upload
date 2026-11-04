import { create } from "zustand";

import { deleteBrowserPkgUpload, stageBrowserPkg } from "../../../api/pkgUpload";
import { stageWithTask } from "../../../lib/browserStaging";
import { isInstallPackagePath } from "../../../lib/pkgDropDedupe";
import { pkgLibraryStore } from "../../../state/pkgLibrary";
import { pushNotification } from "../../../state/notifications";
import { useRemotePlayStore } from "../../../state/remotePlay";
import { useRpQualityStore } from "../../../state/rpQuality";
import { trStatic } from "../../../lib/trStatic";

/** A package dropped on the stream: a path on this computer (the desktop app) or a file the
 *  browser holds (the web UI). */
export interface DroppedPackage {
  id: number;
  name: string;
  /** The console it goes to (the streamed one). */
  host: string;
  path?: string;
  file?: File;
}

interface State {
  /** Dropped and not decided yet: the drop dialog shows these. */
  asking: DroppedPackage[];
  /** To install when the stream ends. */
  held: DroppedPackage[];
  drop: (items: Omit<DroppedPackage, "id">[]) => void;
  /** The dialog's answer for everything it asked about. */
  decide: (when: "later" | "now" | "cancel") => void;
}

let nextId = 1;

/** Install one package on its console, the same way the Install screen does. */
export async function installDropped(p: DroppedPackage): Promise<void> {
  // The library of that console (the stream's), not whichever the app shows now.
  const lib = pkgLibraryStore(p.host).getState();
  if (p.path) {
    await lib.installStream(p.path, p.host);
    return;
  }
  if (!p.file) return;
  // The browser's file goes to the engine first (it cannot read the browser's disk).
  let uploadId: string | null = null;
  try {
    const staged = await stageWithTask(p.file, stageBrowserPkg);
    uploadId = staged.uploadId;
    const r = await lib.installStream(staged.path, p.host, { displayName: staged.filename || p.name });
    if (!r.ok) {
      pushNotification("error", trStatic("rp_drop_failed", "The install didn't complete."), {
        body: `${p.name}: ${r.message ?? ""}`,
      });
    }
  } catch (e) {
    pushNotification("error", trStatic("rp_drop_failed", "The install didn't complete."), {
      body: `${p.name}: ${e instanceof Error ? e.message : String(e)}`,
    });
  } finally {
    if (uploadId) await deleteBrowserPkgUpload(uploadId).catch(() => {});
  }
}

export const useStreamDrops = create<State>((set, get) => ({
  asking: [],
  held: [],
  drop: (items) => {
    const pkgs = items.filter((x) => isInstallPackagePath(x.path ?? x.name));
    if (!pkgs.length) {
      pushNotification(
        "info",
        trStatic("rp_drop_not_pkg", "Only .pkg or .fpkg install packages can be dropped on the stream."),
      );
      return;
    }
    set((s) => ({ asking: [...s.asking, ...pkgs.map((x) => ({ ...x, id: nextId++ }))] }));
  },
  decide: (when) => {
    const items = get().asking;
    set({ asking: [] });
    if (when === "cancel" || !items.length) return;
    if (when === "later") {
      set((s) => ({ held: [...s.held, ...items] }));
      return;
    }
    for (const p of items) void installDropped(p);
  },
}));

/** The stream ended: what was held for then is installed now. */
export function flushHeldDrops(): void {
  const held = useStreamDrops.getState().held;
  if (!held.length) return;
  useStreamDrops.setState({ held: [] });
  pushNotification(
    "info",
    trStatic("rp_drop_flushing", "The stream ended: installing what you dropped on it."),
    { body: held.map((p) => p.name).join(", ") },
  );
  for (const p of held) void installDropped(p);
}

// Held packages wait for the stream's end, wherever the app is then (not only on its screen).
useRemotePlayStore.subscribe((s, prev) => {
  const over = (p: typeof s.phase) => p === "ended" || p === "error";
  const ended = (prev.stream && !s.stream) || (over(s.phase) && !over(prev.phase));
  // A restart with other settings is not the end: the stream is back in a moment.
  if (ended && !useRpQualityStore.getState().switching) flushHeldDrops();
});

// That restart called off, or failed: nothing is streaming now.
useRpQualityStore.subscribe((s, prev) => {
  if (prev.switching && !s.switching && !useRemotePlayStore.getState().stream) flushHeldDrops();
});
