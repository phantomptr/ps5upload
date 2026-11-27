import { useEffect, useState } from "react";
import { RotateCcw } from "lucide-react";

import { restoreCandidates, type RestoreCandidate } from "../../api/titleRestore";
import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { useRestoreBusy, useTitleRestoreStore } from "../../state/titleRestore";

/**
 * Restore to home screen for every game at once (#961): the games the connected PS5 has installed
 * on a drive but no longer lists (Rebuild Database drops games installed from fake packages). Read
 * once per console with the helper's read-only checks; nothing shows when there are none, when the
 * console could not answer, or when its helper is too old.
 */
export function RestoreAllBanner({ host, helperUp }: { host: string; helperUp: boolean }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const start = useTitleRestoreStore((s) => s.start);
  const busy = useRestoreBusy(host);
  const [found, setFound] = useState<RestoreCandidate[]>([]);
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setFound([]);
    if (!helperUp || !host.trim() || busy) return;
    restoreCandidates(host)
      .then((list) => {
        if (!cancelled) setFound(list.filter((c) => c.check.state === "restorable"));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [host, helperUp, busy]);

  if (found.length === 0 && !busy) return null;

  const onRestoreAll = async () => {
    const total = found.reduce((n, c) => n + (c.check.size ?? 0), 0);
    const ok = await confirm({
      title: tr(
        "restore_all_title",
        { count: found.length },
        "Restore {count} games to the home screen?",
      ),
      message: tr(
        "restore_all_body",
        { size: formatBytes(total) },
        "The PS5 installs each game again from the package already on its drive, one after the other ({size} in all), so they come back on the home screen. Nothing is downloaded. Each package is moved aside on its own drive while it installs, a copy of the PS5's game database is saved first, and a package whose install fails is put back where it was. The drive set under Settings › Storage › Installation Location needs room for one game at a time. Fake-package support (kstuff) must be loaded. Don't start these games or turn the PS5 off until it is done.",
      ),
      details: found.map(
        (c) => `${c.title_name} (${c.title_id}) — ${formatBytes(c.check.size ?? 0)}`,
      ),
      confirmLabel: tr("restore_all_action", undefined, "Restore all"),
    });
    if (!ok) return;
    setStarting(true);
    try {
      await start(
        host,
        found.map((c) => ({ titleId: c.title_id, name: c.title_name })),
      );
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, tr("restore_all_action", undefined, "Restore all")), {
        body: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setStarting(false);
    }
  };

  return (
    <div
      className="mb-4 flex flex-wrap items-center gap-3 rounded-2xl border border-[var(--color-warn)]/30 bg-[var(--color-warn)]/10 px-3 py-2 text-xs"
      data-testid="restore-all-banner"
    >
      {dialog}
      <span className="min-w-0 flex-1">
        {busy
          ? tr(
              "restore_all_running",
              { console: hostOf(host) },
              "Games are being restored to the home screen of {console}, one after the other. Each game's page shows how far it is.",
            )
          : tr(
              "restore_all_found",
              { count: found.length },
              "{count} games are installed on this PS5 but are not on its home screen (Rebuild Database removes games installed from fake packages but leaves their files). They can be put back without downloading them again.",
            )}
      </span>
      {!busy && (
        <Button
          size="sm"
          leftIcon={<RotateCcw size={13} />}
          loading={starting}
          onClick={() => void onRestoreAll()}
        >
          {tr("restore_all_action", undefined, "Restore all")}
        </Button>
      )}
    </div>
  );
}
