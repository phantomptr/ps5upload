/**
 * Coverage gate between the Tauri command surface and the browser shim.
 *
 * `browserInvoke.ts` is a hand-maintained switch with no compile-time link
 * to the `#[tauri::command]` list, so the two drift silently: a command
 * added on the Rust side simply falls through to `default:` and throws
 * BrowserUnsupportedError the first time a self-hosted user clicks the
 * button. That is how the whole Cheats/SMB/Backup/SDK surface came to be
 * dead in the web UI while the engine was serving every route (#295).
 *
 * This test makes that drift a CI failure: every command must be either
 * mapped in the shim or listed below with a reason it cannot be.
 */
import { describe, it, expect } from "vitest";

// Read both sides as raw text at transform time. `import.meta.glob` keeps
// this dependency-free — the client has no @types/node, and pulling it in
// just to stat two directories would be a poor trade.
const COMMAND_SOURCES = import.meta.glob("../../src-tauri/src/commands/*.rs", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

const SHIM_SOURCE = Object.values(
  import.meta.glob("./browserInvoke.ts", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>,
)[0];

/**
 * Commands with no browser equivalent, and why. Two kinds live here:
 *
 *  - "native": touches the local machine (host filesystem, OS APIs, the
 *    desktop app's own data dirs). A browser has no such thing.
 *  - "direct-socket": the desktop client opens a TCP connection straight
 *    to the console rather than going through the engine. A browser cannot
 *    open raw sockets, so these need a NEW ENGINE ROUTE before they can
 *    ever be mapped — adding a shim case would not help.
 *
 * Adding a command here is a deliberate statement that the web UI must
 * hide the affordance (see `hideInBrowser` in `layout/navItems.ts` and the
 * `isTauriEnv()` guards at the call sites).
 */
const NATIVE_ONLY: Record<string, string> = {
  // Host filesystem / OS
  read_text_file: "native: reads a host path",
  save_text_file: "native: writes a host path",
  save_archive_zip: "native: host temp-dir staging",
  save_archive_unzip: "native: host temp-dir staging",
  save_archive_make_temp: "native: host temp-dir staging",
  save_archive_cleanup_temp: "native: host temp-dir staging",
  save_archive_backup_finalize: "native: host temp-dir staging",
  save_archive_restore_prepare: "native: host temp-dir staging",
  screenshot_save: "native: writes to the app's screenshot dir",
  screenshot_list: "native: reads the app's screenshot dir",
  screenshot_delete: "native: deletes from the app's screenshot dir",
  screenshot_clear: "native: clears the app's screenshot dir",
  screenshot_open_dir: "native: opens a host file manager",
  screenshot_convert: "native: host image transcode into a temp dir",
  smb_download_file: "native: writes the fetched bytes to a host path",
  usb_list_removable: "native: enumerates host block devices",
  usb_autoloader_install: "native: writes to a host USB volume",
  fs_index_start: "native: indexes the host filesystem",
  fs_index_cancel: "native: indexes the host filesystem",
  fs_index_status: "native: indexes the host filesystem",
  fs_search_index: "native: searches the host filesystem index",
  fs_blake3_hash: "native: hashes a host file",
  crc32_file_get: "native: hashes a host file",

  // Desktop app data / lifecycle
  user_config_load: "native: desktop config file mirror (localStorage in browser)",
  user_config_save: "native: desktop config file mirror (localStorage in browser)",
  user_config_path_resolved: "native: desktop config file path",
  app_data_reset: "native: wipes the desktop app data dir",
  engine_url_get: "native: desktop sidecar URL",
  engine_url_set: "native: desktop sidecar URL",
  toast_push: "native: OS notification",
  keep_awake_set: "native: OS power assertion",
  keep_awake_state: "native: OS power assertion",
  keep_awake_acquire: "native: OS power assertion",
  keep_awake_release: "native: OS power assertion",
  acquire_multicast_lock: "native: Android Wi-Fi multicast lock",
  release_multicast_lock: "native: Android Wi-Fi multicast lock",
  update_check: "native: desktop self-update",
  update_download: "native: desktop self-update",
  changelog_load: "native: reads a bundled resource",
  faq_load: "native: reads a bundled resource",

  // Diagnostics / crash reporting (all host-side files)
  crash_report_save: "native: writes the crash-report dir",
  crash_reports_clear: "native: writes the crash-report dir",
  crash_reports_dir_resolved: "native: host path",
  crash_reports_open_dir: "native: opens a host file manager",
  crash_reports_stats: "native: reads the crash-report dir",
  crash_reports_zip: "native: zips the crash-report dir",
  bug_report_build: "native: assembles a host-side bundle",
  diag_log_append: "native: writes the host diag log",
  diag_log_clear: "native: writes the host diag log",
  diag_log_read_window: "native: reads the host diag log",
  diag_log_stats: "native: reads the host diag log",
  diag_log_open_dir: "native: opens a host file manager",

  // Desktop-side persistence (browser uses localStorage)
  // These two DO run in the browser — api/ps5.ts falls back to
  // localStorage before it ever reaches the shim, so the Tauri command
  // stays desktop-only while the feature still works in a browser.
  upload_queue_load: "native: desktop queue file (browser uses localStorage)",
  upload_queue_save: "native: desktop queue file (browser uses localStorage)",
  payload_playlists_load: "native: desktop playlist file",
  payload_playlists_save: "native: desktop playlist file",
  send_payload_history_load: "native: desktop history file",
  send_payload_history_add: "native: desktop history file",
  send_payload_history_clear: "native: desktop history file",
  resume_txid_lookup: "native: desktop resume-state file",
  resume_txid_remember: "native: desktop resume-state file",
  resume_txid_forget: "native: desktop resume-state file",

  // Payload catalogue: downloads ELFs to host disk, then sends them over a
  // raw socket. The whole /payloads nav entry is hideInBrowser.
  payloads_catalog: "native: caches the catalogue on host disk",
  payloads_releases: "native: talks to the GitHub API from the client",
  payloads_release: "native: talks to the GitHub API from the client",
  payloads_download: "native: downloads an ELF to host disk",
  payloads_local_inventory: "native: reads the host payload dir",
  payloads_local_path: "native: host path",
  payloads_add_custom_repo: "native: desktop repo-config file",
  payloads_remove_custom_repo: "native: desktop repo-config file",
  payload_bundled_path: "native: path of an ELF bundled in the desktop app",

  // Direct TCP to the console — need an engine route before they can work
  payload_probe: "direct-socket: probes the console's mgmt port",
  payload_send: "direct-socket: writes an ELF to the console's loader port",
  companion_probe: "direct-socket: raw TCP connect probe",
  discover_ps5: "direct-socket: mDNS multicast",
  peripheral_eject: "direct-socket: console mgmt port",
  peripheral_bd_on: "direct-socket: console mgmt port",
  peripheral_bd_off: "direct-socket: console mgmt port",
  peripheral_usb_on: "direct-socket: console mgmt port",
  peripheral_usb_off: "direct-socket: console mgmt port",
  ufs_fsck_run: "direct-socket: console mgmt port",
  lwfs_mount_run: "direct-socket: console mgmt port",
  pkg_direct_mount_run: "direct-socket: console mgmt port",
  proc_modules_get: "direct-socket: console mgmt port",
  shell_run_cmd: "direct-socket: console mgmt port",
  net_speed_test_run: "direct-socket: console mgmt port",
  appdb_query_get: "direct-socket: console mgmt port",
  heal_appmeta: "direct-socket: console mgmt port",
  title_meta_fetch: "native: fetches artwork over https from the client",
};

function tauriCommands(): string[] {
  const names: string[] = [];
  for (const src of Object.values(COMMAND_SOURCES)) {
    const lines = src.split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (!lines[i].includes("#[tauri::command]")) continue;
      // The attribute and the fn can be separated by doc comments or
      // other attributes; the fn is always within a few lines.
      for (let j = i + 1; j < Math.min(i + 5, lines.length); j++) {
        const m = /(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/.exec(lines[j]);
        if (m) {
          names.push(m[1]);
          break;
        }
      }
    }
  }
  return [...new Set(names)].sort();
}

function mappedCommands(): Set<string> {
  return new Set(
    [...SHIM_SOURCE.matchAll(/^\s+case "([a-z0-9_]+)":/gm)].map((m) => m[1]),
  );
}

describe("browser shim covers the Tauri command surface", () => {
  const commands = tauriCommands();
  const mapped = mappedCommands();

  it("finds both sides (guards against a broken parse)", () => {
    expect(commands.length).toBeGreaterThan(200);
    expect(mapped.size).toBeGreaterThan(100);
  });

  it("every command is either mapped or documented as native-only", () => {
    const undeclared = commands.filter(
      (c) => !mapped.has(c) && !(c in NATIVE_ONLY),
    );
    expect(
      undeclared,
      `These Tauri commands are unreachable from the browser and undocumented.\n` +
        `Either add a case to browserInvoke.ts (if the engine serves a route for it)\n` +
        `or add it to NATIVE_ONLY with a reason:\n  ${undeclared.join("\n  ")}`,
    ).toEqual([]);
  });

  it("the native-only list has no stale entries", () => {
    const known = new Set(commands);
    // A command that is both mapped and listed native-only is a
    // contradiction; one that no longer exists is dead weight.
    const stale = Object.keys(NATIVE_ONLY).filter(
      (c) => !known.has(c) || mapped.has(c),
    );
    expect(
      stale,
      `Stale NATIVE_ONLY entries (command gone, or now mapped): ${stale.join(", ")}`,
    ).toEqual([]);
  });
});
