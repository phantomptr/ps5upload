import { useCallback } from "react";

import { useConfirm } from "../../components/ConfirmDialog";
import { meshAnnounce } from "../../api/mesh";
import { hostOf } from "../../lib/addr";
import { getAppVersion } from "../../lib/appVersion";
import { busyText, replaceCheck } from "../../lib/helperGuard";
import { transferScreenBusy } from "../../lib/ps5Transfers";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { meshFor, useMeshStore } from "../../state/mesh";

/**
 * Asks before this app's helper replaces the one running on `host` (8.0.0): when other devices
 * are busy there (their work stops until the helper is back), and when the running helper is
 * newer than this app's (a downgrade, for every device). Then tells the other devices the helper
 * is about to restart. True: go ahead and send.
 */
export function useHelperReplaceGuard(): { guard: (host: string) => Promise<boolean>; dialog: React.ReactNode } {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const guard = useCallback(
    async (host: string) => {
      // That console's own state: Mesh Drive sends to one that is not the selected one.
      const rt = useConnectionStore.getState().runtimeByHost[hostOf(host) || "_"];
      // Nothing is running there: nobody to warn.
      if (rt?.payloadStatus !== "up") return true;
      const state = meshFor(useMeshStore.getState(), host).state;
      const app = await getAppVersion().catch(() => null);
      const check = replaceCheck(state, rt.payloadVersion, app);
      if (check.downgrade) {
        const { running, ours } = check.downgrade;
        const ok = await confirm({
          title: tr(
            "mesh_downgrade_title",
            { running, ours },
            `Replace helper v${running} with this app's older v${ours}?`,
          ),
          message: tr(
            "mesh_downgrade_body",
            undefined,
            "Another device sent a newer helper. Replacing it takes its newer features and fixes away from every device that uses this console. Updating this app is the better fix.",
          ),
          confirmLabel: tr("mesh_downgrade_confirm", undefined, "Replace with the older helper"),
          destructive: true,
        });
        if (!ok) return false;
      }
      if (check.busy.length) {
        const ok = await confirm({
          title: tr("mesh_busy_title", undefined, "Other devices are using this console"),
          message: tr(
            "mesh_busy_body",
            { work: busyText(check.busy) },
            `${busyText(check.busy)}. Sending the helper restarts it: their work stops, and uploads pick up where they left off once it is back.`,
          ),
          confirmLabel: tr("mesh_busy_confirm", undefined, "Send anyway"),
          destructive: true,
        });
        if (!ok) return false;
      }
      // This app's own uploads and copies stop with the helper too.
      if (transferScreenBusy(host)) {
        const ok = await confirm({
          title: tr("helper_replace_own_busy_title", undefined, "A transfer is running on this console"),
          message: tr(
            "helper_replace_own_busy_body",
            undefined,
            "Sending the helper restarts it, and this transfer stops. Send it when the transfer has finished, or send it now and start the transfer again.",
          ),
          confirmLabel: tr("mesh_busy_confirm", undefined, "Send anyway"),
          destructive: true,
        });
        if (!ok) return false;
      }
      if (state) {
        // Best effort: the others hear it on their next poll, before the helper goes.
        await meshAnnounce(host, "restart", app ? `sending helper v${app}` : "sending the helper").catch(() => false);
      }
      return true;
    },
    [confirm, tr],
  );
  return { guard, dialog };
}
