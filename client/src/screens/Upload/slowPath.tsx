// A short warning with a Continue button before a large upload over a slow path: a relayed
// Tailscale connection, or phone data.

import { useCallback } from "react";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { onCellularData, slowPathWarning, tailnetVia } from "../../api/tailnet";
import { formatBytes } from "../../lib/format";

export function useSlowPathGuard(): {
  /** True to go on: no warning was due, or the user chose Continue. */
  guard: (host: string, totalBytes: number | null | undefined) => Promise<boolean>;
  dialog: React.ReactNode;
} {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const guard = useCallback(
    async (host: string, totalBytes: number | null | undefined) => {
      const bytes = totalBytes ?? 0;
      if (bytes <= 0) return true;
      const cellular = onCellularData();
      // Only asked for a large upload: the cheap check first.
      const via = slowPathWarning(bytes, "relayed", cellular) ? (await tailnetVia(host)).via : "lan";
      const why = slowPathWarning(bytes, via, cellular);
      if (!why) return true;
      return confirm({
        title: tr("slow_path_title", undefined, "This upload may be slow"),
        message:
          why === "cellular"
            ? tr(
                "slow_path_cellular",
                { size: formatBytes(bytes) },
                "You're on phone data. Sending {size} may take a long time and use a lot of your data plan.",
              )
            : tr(
                "slow_path_relayed",
                { size: formatBytes(bytes) },
                "This PS5 is reached through Tailscale's relay servers, which are slow for {size}. At home, or with a direct Tailscale connection, it is much faster.",
              ),
        confirmLabel: tr("slow_path_continue", undefined, "Continue"),
      });
    },
    [confirm, tr],
  );
  return { guard, dialog };
}
