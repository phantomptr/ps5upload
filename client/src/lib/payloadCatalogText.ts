// A catalogue payload's purpose and blurb in the user's language. The Rust catalogue keeps its own
// English text, which stands in for an entry this build has no words for (a newer catalogue).

import type { PayloadInfo } from "../api/ps5";

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** By catalogue id. Written out, so the checks for unused and missing keys can read them. */
export const CATALOG_KEYS: Record<string, [role: string, description: string]> = {
  "elfldr": ["payload_role_elfldr", "payload_desc_elfldr"],
  "kstuff-echostretch": ["payload_role_kstuff_echostretch", "payload_desc_kstuff_echostretch"],
  "kstuff-lite-drakmor": ["payload_role_kstuff_lite_drakmor", "payload_desc_kstuff_lite_drakmor"],
  "shadowmountplus": ["payload_role_shadowmountplus", "payload_desc_shadowmountplus"],
  "etahen": ["payload_role_etahen", "payload_desc_etahen"],
  "ftpsrv": ["payload_role_ftpsrv", "payload_desc_ftpsrv"],
  "websrv": ["payload_role_websrv", "payload_desc_websrv"],
  "ps5-app-dumper": ["payload_role_ps5_app_dumper", "payload_desc_ps5_app_dumper"],
  "itemzflow": ["payload_role_itemzflow", "payload_desc_itemzflow"],
  "cheatrunner": ["payload_role_cheatrunner", "payload_desc_cheatrunner"],
  "shsrv": ["payload_role_shsrv", "payload_desc_shsrv"],
  "webkit-autoloader": ["payload_role_webkit_autoloader", "payload_desc_webkit_autoloader"],
  "payload-manager": ["payload_role_payload_manager", "payload_desc_payload_manager"],
  "lz4-compression": ["payload_role_lz4_compression", "payload_desc_lz4_compression"],
  "np-fake-signin": ["payload_role_np_fake_signin", "payload_desc_np_fake_signin"],
  "garlic-worker": ["payload_role_garlic_worker", "payload_desc_garlic_worker"],
  "garlic-savemgr": ["payload_role_garlic_savemgr", "payload_desc_garlic_savemgr"],
  "klogsrv": ["payload_role_klogsrv", "payload_desc_klogsrv"],
  "nanodns": ["payload_role_nanodns", "payload_desc_nanodns"],
  "backpork": ["payload_role_backpork", "payload_desc_backpork"],
  "hush": ["payload_role_hush", "payload_desc_hush"],
  "remoteplay-unlock": ["payload_role_remoteplay_unlock", "payload_desc_remoteplay_unlock"],
  "ps5debug-ng": ["payload_role_ps5debug_ng", "payload_desc_ps5debug_ng"],
};

/** The role and description to show for `p`. */
export function catalogText(
  tr: Tr,
  p: Pick<PayloadInfo, "id" | "role" | "description" | "is_custom" | "repo_owner" | "repo_name">,
): { role: string; description: string } {
  if (p.is_custom) {
    return {
      role: tr("payload_role_custom", undefined, p.role),
      description: tr(
        "payload_desc_custom",
        { owner: p.repo_owner, name: p.repo_name },
        p.description,
      ),
    };
  }
  const keys = CATALOG_KEYS[p.id];
  if (!keys) return { role: p.role, description: p.description };
  return { role: tr(keys[0], undefined, p.role), description: tr(keys[1], undefined, p.description) };
}
