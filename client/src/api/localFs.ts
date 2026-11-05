// Real-path local filesystem browse + Android all-files-access, backing
// the in-app file/folder picker (LocalPathPicker). On Android the native
// dialog returns content:// URIs the engine can't read, so we browse the
// real filesystem here instead. Commands live in
// client/src-tauri/src/commands/local_fs.rs.
//
// In a browser session (no Tauri) this instead browses the ENGINE's own
// filesystem via `invokeLogged` → `browserInvoke` → the engine's
// /api/local/* routes (see engine/crates/ps5upload-engine/src/local_fs.rs).

import { invoke } from "../lib/invokeLogged";

export interface LocalEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

/** How many entries the in-app browser asks for at a time: a folder of 100k entries as one
 *  reply is many MB of JSON and more rows than a window can hold anyway. */
const LIST_PAGE = 2000;

export const localFs = {
  /** List a real directory (dirs first, then files, alphabetical). */
  listDir: (path: string) => invoke<LocalEntry[]>("local_list_dir", { path }),
  /** One page of `listDir` from `offset`; `next` is where the following one starts, if any. */
  listDirPage: async (path: string, offset: number): Promise<{ entries: LocalEntry[]; next: number | null }> => {
    const entries = await invoke<LocalEntry[]>("local_list_dir", { path, offset, limit: LIST_PAGE });
    return { entries, next: entries.length === LIST_PAGE ? offset + LIST_PAGE : null };
  },
  /** Seed roots for the browser (Android: shared storage + removable). */
  storageRoots: () => invoke<string[]>("local_storage_roots"),
  /** Whether the app may read shared storage (always true off Android). */
  accessGranted: () => invoke<boolean>("storage_access_granted"),
  /** Ask for it: Android's prompt on Android 7 to 10, the All-files-access settings page on 11+
   *  (no-op off Android). */
  requestAccess: async (): Promise<void> => {
    const storage = storageBridge();
    if (storage?.usesPrompt()) {
      storage.request();
      return;
    }
    return invoke<void>("request_storage_access");
  },
  /** Android's public Downloads folder for this user. */
  downloadsDir: () => invoke<string>("local_downloads_dir"),
};

interface StorageBridge {
  usesPrompt(): boolean;
  request(): void;
}

/** MainActivity's (step 15 of android-postinit-patch.sh). */
function storageBridge(): StorageBridge | undefined {
  if (typeof window === "undefined") return undefined;
  return (window as unknown as { PS5UploadStorage?: StorageBridge }).PS5UploadStorage;
}

/** Android 7 to 10: the storage permission is a prompt in the app, not a page in Settings. */
export function storageAccessUsesPrompt(): boolean {
  try {
    return !!storageBridge()?.usesPrompt();
  } catch {
    return false;
  }
}
