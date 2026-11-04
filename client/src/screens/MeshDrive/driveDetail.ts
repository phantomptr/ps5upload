// What the engine says about a folder or a link, in the user's language. The engine's own
// sentences come with a code and what goes in them; a raw error (a file's, say) has no code and
// shows as it is.

import type { Translator } from "../../state/lang";

export function driveDetailText(
  tr: Translator,
  code: string | undefined,
  args: Record<string, string> | undefined,
  fallback: string,
): string {
  const a = args ?? {};
  switch (code) {
    case "side_no_marker":
      return tr(
        "drive_detail_side_no_marker",
        a,
        "{path} is missing its .ps5upload-mesh file. Was it unplugged or moved?",
      );
    case "hub_no_marker":
      return tr(
        "drive_detail_hub_no_marker",
        a,
        "The drive's folder on the console ({path}) is missing its .ps5upload-mesh file.",
      );
    case "install_running":
      return tr(
        "drive_detail_install_running",
        a,
        "An install is running on the console. Syncing waits for it.",
      );
    case "other_syncing":
      return tr("drive_detail_other_syncing", a, "{name} is syncing it.");
    case "lock_taken":
      return tr(
        "drive_detail_lock_taken",
        a,
        "Another device took over this drive. This sync stopped.",
      );
    case "helper_old":
      return tr(
        "drive_detail_helper_old",
        a,
        "The console's helper is older than 8.0.0. Send it this app's helper.",
      );
    case "pass_stopped":
      return tr(
        "drive_detail_pass_stopped",
        a,
        "The last sync stopped unexpectedly. It will try again.",
      );
    case "changed_here":
      return tr(
        "drive_detail_changed_here",
        a,
        "{path} changed on this device while it was syncing. It syncs next time.",
      );
    case "changed_there":
      return tr(
        "drive_detail_changed_there",
        a,
        "{path} changed on {console} while it was syncing. It syncs next time.",
      );
    case "confirm_deletes":
      return tr(
        "drive_confirm_text",
        a,
        "{count} files would be deleted everywhere. Nothing was deleted yet.",
      );
    case "too_many_files":
      return tr(
        "drive_detail_too_many_files",
        a,
        "More than {limit} files: too many for a Mesh Drive.",
      );
    case "list_unreadable":
      return tr(
        "drive_detail_list_unreadable",
        a,
        "This version cannot read the console's drive list. Update this app.",
      );
    case "link_record_damaged":
      return tr(
        "drive_detail_link_record_damaged",
        a,
        "The link's record on {console} is damaged.",
      );
    case "too_many_for_console":
      return tr(
        "drive_detail_too_many_for_console",
        a,
        "Too many files for a drive folder on a second console.",
      );
    default:
      return fallback;
  }
}
