import { useEffect, useRef } from "react";

import {
  answerInstallAsk,
  INSTALL_ASK_TIMEOUT_MS,
  registerInstallAskHost,
  useInstallAskStore,
} from "../state/installAsk";
import { useConfirm } from "./ConfirmDialog";

/** Shows what a running install asks the person (see state/installAsk). Mounted once, in the shell. */
export function InstallAskHost() {
  const { confirm, dialog } = useConfirm();
  const current = useInstallAskStore((s) => s.pending[0] ?? null);
  const shown = useRef<unknown>(null);

  useEffect(() => registerInstallAskHost(), []);

  useEffect(() => {
    if (!current || shown.current === current) return;
    shown.current = current;
    void confirm({
      title: current.title,
      message: current.message,
      confirmLabel: current.confirmLabel,
      destructive: current.destructive,
      cancelAfterMs: INSTALL_ASK_TIMEOUT_MS,
    }).then((yes) => answerInstallAsk(yes));
  }, [current, confirm]);

  return <>{dialog}</>;
}
