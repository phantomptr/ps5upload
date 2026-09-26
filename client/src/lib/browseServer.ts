// Look around a saved server in the in-app browser, with Install (a .pkg streams straight to
// the console) and Send to PS5 (opens Upload with the file picked) on each file.

import type { NavigateFunction } from "react-router";

import { useConnectionStore } from "../state/connection";
import { pickLocalPath } from "../state/localPicker";
import { pushNotification } from "../state/notifications";
import { pkgLibraryStore } from "../state/pkgLibrary";
import { useUploadStore } from "../state/upload";

export function browseServer(
  c: { id: string; name: string },
  navigate: NavigateFunction,
  needPs5Message: string,
): void {
  void pickLocalPath({
    mode: "file",
    title: c.name,
    source: { connectionId: c.id },
    actions: {
      onInstall: (path) => {
        const host = useConnectionStore.getState().host?.trim();
        if (!host) {
          pushNotification("error", needPs5Message);
          return;
        }
        void pkgLibraryStore(host).getState().installStream(path, host);
        navigate("/install-package");
      },
      onSend: (path) => {
        void useUploadStore.getState().pickFile(path);
        navigate("/upload");
      },
    },
  });
}
