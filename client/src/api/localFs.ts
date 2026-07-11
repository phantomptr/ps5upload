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

export const localFs = {
  /** List a real directory (dirs first, then files, alphabetical). */
  listDir: (path: string) => invoke<LocalEntry[]>("local_list_dir", { path }),
  /** Seed roots for the browser (Android: shared storage + removable). */
  storageRoots: () => invoke<string[]>("local_storage_roots"),
  /** Whether all-files access is granted (always true off Android). */
  accessGranted: () => invoke<boolean>("storage_access_granted"),
  /** Open the system All-files-access settings page (no-op off Android). */
  requestAccess: () => invoke<void>("request_storage_access"),
};
