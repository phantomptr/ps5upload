import { useConfirm } from "../components/ConfirmDialog";
import { isAndroid } from "../lib/platform";
import { useTr } from "../state/lang";
import { anyWorkRunning, runningWorkName } from "../state/runningWork";

/** Android replaces the app to update it, and the engine lives inside the app: an upload, or a
 *  package the PS5 is streaming from the phone, ends there and then. Desktop's automatic install
 *  waits for idle (state/update.ts); a person pressing Install on a phone gets asked. */
export function useApkInstallGuard(): { confirmInstall: () => Promise<boolean>; guardDialog: React.ReactNode } {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const confirmInstall = async () => {
    if (!isAndroid() || !anyWorkRunning()) return true;
    const name = runningWorkName();
    return confirm({
      title: tr("update_apk_busy_title", undefined, "Stop what's running and update?"),
      message: name
        ? tr(
            "update_apk_busy_named",
            { name },
            "{name} is still running. Installing the update closes the app, and that stops it.",
          )
        : tr(
            "update_apk_busy",
            undefined,
            "A transfer or install is still running. Installing the update closes the app, and that stops it.",
          ),
      confirmLabel: tr("update_apk_busy_confirm", undefined, "Update anyway"),
      destructive: true,
    });
  };
  return { confirmInstall, guardDialog: dialog };
}
