import { useState } from "react";
import { Download, FileText } from "lucide-react";
import { Button, Card } from "../../components";
import { profileInfo, profileNpState, type NpState } from "../../api/ps5";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { useConsoleLabel } from "../../state/roster";
import { getAppVersion } from "../../lib/appVersion";
import { browserDownloadText } from "../../lib/browserDownload";
import { isTauriEnv } from "../../lib/tauriEnv";
import { userSlots } from "./userSlots";
import { userInfoFileName, userInfoSummary } from "./userInfoSummary";
import { humanizeCaught } from "../../lib/humanizeError";

/** Back up the console's user information: a text file with every user, account ID and sign-in
 *  state, and how to give a new user an old account ID (its saves follow the ID). */
export function UserInfoBackup({ addr }: { addr: string }) {
  const tr = useTr();
  const consoleName = useConsoleLabel(addr);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function backUp() {
    setBusy(true);
    setDone(null);
    setError(null);
    try {
      // Read fresh, not what the screen showed: the file is a record of the console now.
      const info = await profileInfo(addr);
      const np = new Map<number, NpState | { error: string }>();
      const slotOf = userSlots(info.users, info.slots);
      for (const u of info.users) {
        const slot = slotOf.get(u.uid);
        if (!slot) continue;
        try {
          np.set(u.uid, await profileNpState(slot.slot, u.uid, addr));
        } catch (e) {
          np.set(u.uid, { error: humanizeCaught(e) });
        }
      }
      const conn = useConnectionStore.getState();
      const at = new Date();
      const text = userInfoSummary({
        at,
        appVersion: await getAppVersion().catch(() => ""),
        consoleName,
        consoleAddr: conn.host.trim(),
        payloadVersion: conn.payloadVersion,
        kernel: conn.ps5Kernel,
        info,
        np,
      });
      const fileName = userInfoFileName(consoleName, at);
      if (!isTauriEnv()) {
        browserDownloadText(fileName, text);
        setDone(fileName);
        return;
      }
      const { pickSaveDest } = await import("../../lib/pickSaveDest");
      const { writeTextFileToPath } = await import("../../lib/saveTextFile");
      const dest = await pickSaveDest({
        defaultPath: fileName,
        filters: [{ name: "Text", extensions: ["txt"] }],
      });
      if (!dest) return;
      await writeTextFileToPath(dest, text, fileName);
      setDone(dest);
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card>
      <div className="mb-1 flex items-center gap-2">
        <FileText size={16} className="text-[var(--color-accent)]" />
        <h2 className="text-sm font-semibold">{tr("accounts_backup_title", undefined, "Back up user information")}</h2>
      </div>
      <p className="mb-4 text-xs text-[var(--color-muted)]">
        {tr(
          "accounts_backup_description",
          undefined,
          "Download a text file with every user on this PS5: names, user and account IDs, how each account is activated and signed in, and how to give a new user an old account ID after a reset or crash (saves follow the account ID). Keep it private.",
        )}
      </p>
      <Button
        variant="secondary"
        size="sm"
        leftIcon={<Download size={14} />}
        loading={busy}
        disabled={busy}
        onClick={() => void backUp()}
        data-testid="accounts-backup"
      >
        {tr("accounts_backup_button", undefined, "Download user information (.txt)")}
      </Button>
      {done && (
        <p className="mt-3 break-all text-xs text-[var(--color-muted)]" data-testid="accounts-backup-done">
          {tr("accounts_backup_saved", { path: done }, `Saved: ${done}`)}
        </p>
      )}
      {error && (
        <p className="mt-3 text-xs text-[var(--color-bad)]" role="alert">
          {tr("accounts_backup_failed", { error }, `Could not back up the user information: ${error}`)}
        </p>
      )}
    </Card>
  );
}
