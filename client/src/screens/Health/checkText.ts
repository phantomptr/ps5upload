// The health report's sentences in the user's language. The engine says which sentence each one
// is (`id`, `detail_id`, `remedy_id`) next to its English; a check this does not know, or one from
// an older engine, shows that English.

import type { HealthCheck } from "../../api/ps5";
import type { Translator } from "../../state/lang";

export function checkTitle(tr: Translator, c: HealthCheck): string {
  switch (c.id) {
    case "mgmt_reachable":
      return tr("health_t_mgmt", undefined, "Helper is reachable");
    case "payload_version_match":
      return tr("health_t_version", undefined, "Helper matches this app");
    case "sony_symbols_resolved":
      return tr("health_t_registry", undefined, "System registry is readable");
    case "firmware_detected":
      return tr("health_t_firmware", undefined, "Firmware detected");
    case "rp_service_enabled":
      return tr("health_t_rp", undefined, "Remote Play is enabled");
    case "rp_account_activated":
      return tr("health_t_account", undefined, "Account is usable");
    case "internal_free_space":
      return tr("health_t_storage", undefined, "Console storage");
    case "tool_dirs_present":
      return tr("health_t_dirs", undefined, "Tool folders exist");
    case "junk_files":
      return tr("health_t_junk", undefined, "Leftover files");
    case "clock_sane":
      return tr("health_t_clock", undefined, "Console clock");
    default:
      return c.title;
  }
}

export function checkDetail(tr: Translator, c: HealthCheck): string {
  const v = c.values ?? {};
  switch (c.detail_id) {
    case "mgmt_ok":
      return tr("health_d_mgmt_ok", v, "The helper answered on {addr}.");
    case "mgmt_failed":
      return tr("health_d_mgmt_failed", v, "No answer from {addr}: {error}");
    case "version":
      return tr("health_d_version", v, "Console helper {helper}, this app {app}.");
    case "symbols_ok":
      return tr("health_d_symbols_ok", undefined, "The helper can read the console's system registry.");
    case "symbols_bad":
      return tr("health_d_symbols_bad", v, "The system registry can't be read (error {err}).");
    case "fw":
      return tr("health_d_fw", v, "System software {version}.");
    case "fw_none":
      return tr("health_d_fw_none", undefined, "The console did not report a readable firmware version.");
    case "rp_on":
      return tr("health_d_rp_on", undefined, "Remote Play is switched on.");
    case "rp_off":
      return tr("health_d_rp_off", undefined, "Remote Play is switched off on this console.");
    case "account_ok":
      return tr("health_d_account_ok", undefined, "The signed-in account has an id and is PSN-linked.");
    case "account_none":
      return tr("health_d_account_none", undefined, "No usable account is signed in on this console.");
    case "readiness_failed":
      return tr("health_d_readiness_failed", v, "Could not read console readiness: {error}");
    case "storage":
      return tr("health_d_storage", v, "{free} free of {total} ({pct}%).");
    case "storage_none":
      return tr("health_d_storage_none", undefined, "The console did not report a storage size.");
    case "storage_failed":
      return tr("health_d_storage_failed", v, "Could not read storage: {error}");
    case "dirs_ok":
      return tr("health_d_dirs_ok", undefined, "All folders ps5upload needs are present.");
    case "dirs_missing":
      return tr("health_d_dirs_missing", v, "Missing or unreadable: {dirs}.");
    case "junk_unread":
      return tr("health_d_junk_unread", undefined, "Could not read every tool folder, so leftovers were not counted.");
    case "junk_none":
      return tr("health_d_junk_none", undefined, "No leftover files from interrupted transfers.");
    case "junk_found":
      return tr("health_d_junk_found", v, "Leftover files from interrupted work: {count}, {size}.");
    case "clock_skew":
      return tr("health_d_clock_skew", v, "The console's clock differs from this computer by {skew} s.");
    case "clock_bad":
      return tr("health_d_clock_bad", undefined, "The console reported a date that could not be interpreted.");
    case "clock_silent":
      return tr("health_d_clock_silent", v, "The console did not report its clock: {error}.");
    case "clock_failed":
      return tr("health_d_clock_failed", v, "Could not read the console clock: {error}");
    default:
      return c.detail;
  }
}

export function checkRemedy(tr: Translator, c: HealthCheck): string {
  switch (c.remedy_id) {
    case "load_helper":
      return tr("health_r_load_helper", undefined, "Load the ps5upload payload on your console, then scan again. Everything below depends on this.");
    case "resend":
      return tr("health_r_resend", undefined, "Send the payload again from this version of the app. A helper of another version is the most common reason a feature seems to do nothing.");
    case "registry":
      return tr("health_r_registry", undefined, "Several features read the registry and do nothing while this fails. Reload the payload. If it stays like this, report it.");
    case "rp_on":
      return tr("health_r_rp_on", undefined, "Turn Remote Play on.");
    case "account":
      return tr("health_r_account", undefined, "Remote Play needs a signed-in account. Sign in on the console, or activate the profile from the Profile screen.");
    case "space_low":
      return tr("health_r_space_low", undefined, "Very little space left. Installs and transfers will fail. Delete something before continuing.");
    case "space_tight":
      return tr("health_r_space_tight", undefined, "Space is getting tight. Large installs may fail.");
    case "create_dirs":
      return tr("health_r_create_dirs", undefined, "Create the missing folders.");
    case "delete_junk":
      return tr("health_r_delete_junk", undefined, "Delete them. Only files ps5upload created are removed.");
    case "clock":
      return tr("health_r_clock", undefined, "A wrong clock breaks licence checks and makes logs hard to read. Set it from this computer.");
    default:
      return c.remedy;
  }
}
