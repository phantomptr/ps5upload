// iOS: a file picked in the Files sheet is copied into the app's temporary Inbox folder
// (lib/pickPath.ts), and nothing ever removed it: a 40 GB game took 40 GB more, invisible in Files
// and out of the person's reach, until iOS purged it at a moment of its choosing. So:
// - at launch, everything there goes except what the queue still needs (nothing else can be in
//   use yet: a pick on a screen does not survive a restart, and the engine's jobs do not either);
// - a queued file goes once its item is done or removed.

import { invoke } from "../lib/invokeLogged";
import { isIOS } from "../lib/platform";
import { isTauriEnv } from "../lib/tauriEnv";
import { useUploadQueueStore, type QueueItem } from "./uploadQueue";

const IN_INBOX = /\/[^/]*-Inbox\//;

/** The picked copies the queue still needs. Exported for tests. */
export function inboxPathsNeeded(items: readonly Pick<QueueItem, "sourcePath" | "status">[]): Set<string> {
  return new Set(
    items.filter((it) => it.status !== "done" && IN_INBOX.test(it.sourcePath)).map((it) => it.sourcePath),
  );
}

/** Once, at start-up. Best effort: the worst outcome is what iOS would do anyway. */
export function installIosInbox(): void {
  if (!isIOS() || !isTauriEnv()) return;
  let swept = false;
  let needed = new Set<string>();
  const onChange = (s: { loaded: boolean; items: QueueItem[] }) => {
    // Before the queue is loaded its empty list means nothing.
    if (!s.loaded) return;
    const now = inboxPathsNeeded(s.items);
    if (!swept) {
      swept = true;
      void invoke("ios_inbox_sweep", { keep: [...now] }).catch(() => {});
    }
    for (const path of needed) {
      if (!now.has(path)) void invoke("ios_inbox_release", { path }).catch(() => {});
    }
    needed = now;
  };
  useUploadQueueStore.subscribe(onChange);
  onChange(useUploadQueueStore.getState());
}
