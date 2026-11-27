import { useCallback, useEffect, useState } from "react";
import { RotateCcw, Undo2, CheckCircle2 } from "lucide-react";

import { restoreCheck, restoreFinish, type RestoreReply } from "../../api/titleRestore";
import { Button, WarningCard } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { formatBytes } from "../../lib/format";
import {
  restoreConfirmText,
  restoreEnded,
  restoreErrorText,
  restoreNeeded,
  restoreStateText,
} from "../../lib/titleRestore";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { useRestoreRun, useTitleRestoreStore } from "../../state/titleRestore";

/**
 * Restore to home screen (#961): what the console says about a game that is installed on one of
 * its drives but missing from its home screen (Rebuild Database drops games installed from fake
 * packages), read with the helper's read-only check. The page's Play is replaced by this.
 *
 * `null` while unknown, or when the console could not answer or its helper is too old: then
 * nothing is shown and Play stays as it was (the helper refuses a launch of a title its database
 * does not know, so Play cannot unsettle the console either way).
 */
export function useRestoreCheck(
  host: string,
  titleId: string | undefined,
): { reply: RestoreReply | null; refresh: () => void } {
  const [reply, setReply] = useState<RestoreReply | null>(null);
  const [tick, setTick] = useState(0);
  const run = useRestoreRun(host, titleId ?? "");
  const phase = run?.phase;
  useEffect(() => {
    let cancelled = false;
    setReply(null);
    if (!host.trim() || !titleId || !/^[A-Za-z]{4}\d{5}$/.test(titleId)) return;
    restoreCheck(host, titleId)
      .then((r) => {
        if (!cancelled) setReply(r);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
    // A finished restore changes the answer: read it again then.
  }, [host, titleId, tick, phase]);
  const refresh = useCallback(() => setTick((n) => n + 1), []);
  return { reply, refresh };
}

export function RestoreHomeScreenCard({
  host,
  titleId,
  name,
  reply,
  onChanged,
}: {
  host: string;
  titleId: string;
  name: string;
  reply: RestoreReply | null;
  onChanged: () => void;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const start = useTitleRestoreStore((s) => s.start);
  const run = useRestoreRun(host, titleId);
  const [busy, setBusy] = useState<"start" | "finish" | "rollback" | null>(null);

  const running = !!run && !restoreEnded(run.phase);
  if (!running && !restoreNeeded(reply)) return null;

  const onRestore = async () => {
    if (!reply) return;
    const ok = await confirm({
      title: tr("restore_confirm_title", { name }, "Restore {name} to the home screen?"),
      message: restoreConfirmText(reply, name),
      confirmLabel: tr("restore_action", undefined, "Restore to home screen"),
    });
    if (!ok) return;
    setBusy("start");
    try {
      await start(host, [{ titleId, name }]);
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, name), {
        body: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(null);
    }
  };

  const onFinish = async (rollback: boolean) => {
    const ok = await confirm(
      rollback
        ? {
            title: tr("restore_rollback_title", { name }, "Put {name}'s package back?"),
            message: tr(
              "restore_rollback_body",
              undefined,
              "The package goes back to where it was before the restore, and the game stays off the home screen. Only do this when the PS5 shows no download or install of this game in progress.",
            ),
            confirmLabel: tr("restore_rollback", undefined, "Put back"),
          }
        : {
            title: tr("restore_finish_title", { name }, "Is {name} back on the home screen?"),
            message: tr(
              "restore_finish_body",
              undefined,
              "When the game shows on the PS5's home screen and its install has finished, the copy PS5Upload moved aside is deleted. If the PS5 does not list the game yet, nothing is deleted.",
            ),
            confirmLabel: tr("restore_finish", undefined, "Finish"),
          },
    );
    if (!ok) return;
    setBusy(rollback ? "rollback" : "finish");
    try {
      const r = await restoreFinish(host, titleId, rollback ? { rollback: true } : { installEnded: true });
      const body =
        r.state === "restored"
          ? tr("restore_done", { name }, "{name} is back on the home screen.")
          : r.state === "rolled_back"
            ? tr("restore_put_back", { name }, "{name}'s package is back where it was.")
            : r.state === "nothing_pending"
              ? tr("restore_nothing_pending", { name }, "There was nothing left to finish for {name}.")
              : tr(
                  "restore_not_yet",
                  { name },
                  "{name} is not on the home screen yet, so nothing was deleted. Wait for the PS5 to finish, or put the package back.",
                );
      pushNotification(r.state === "restored" || r.state === "rolled_back" ? "success" : "info", withConsolePrefix(host, name), {
        body,
      });
      onChanged();
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, name), {
        body: restoreErrorText(e instanceof Error ? e.message : String(e), name),
      });
    } finally {
      setBusy(null);
    }
  };

  let detail: string | null;
  if (running) {
    const m = run?.install?.metrics;
    const done = m?.total_bytes ? Math.min(m.served_bytes ?? 0, m.total_bytes) : 0;
    detail =
      run?.phase === "queued"
        ? tr("restore_queued", { name }, "{name} is waiting for the restore before it.")
        : run?.phase === "finishing"
          ? tr("restore_finishing", { name }, "The PS5 has installed {name}; checking that it is back on the home screen.")
          : m?.total_bytes
            ? tr(
                "restore_installing_progress",
                { name, done: formatBytes(done), total: formatBytes(m.total_bytes) },
                "The PS5 is installing {name} again from its package: {done} of {total}. Don't start the game or turn the PS5 off until it is done.",
              )
            : tr(
                "restore_installing",
                { name },
                "The PS5 is installing {name} again from its package. Don't start the game or turn the PS5 off until it is done.",
              );
  } else {
    detail = restoreStateText(reply, name);
  }
  const state = reply?.state;
  return (
    <div className="mb-3" data-testid="restore-home-screen">
      {dialog}
      <WarningCard
        title={
          running
            ? tr("restore_running_title", { name }, "Restoring {name} to the home screen")
            : state === "pending"
              ? tr("restore_pending_title", { name }, "The restore of {name} did not finish")
              : tr("restore_missing_title", { name }, "{name} is not on the PS5's home screen")
        }
        detail={detail ?? undefined}
        action={
          running ? undefined : state === "restorable" ? (
            <Button
              size="sm"
              leftIcon={<RotateCcw size={13} />}
              loading={busy === "start"}
              disabled={busy !== null}
              onClick={() => void onRestore()}
            >
              {tr("restore_action", undefined, "Restore to home screen")}
            </Button>
          ) : state === "pending" ? (
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                leftIcon={<CheckCircle2 size={13} />}
                loading={busy === "finish"}
                disabled={busy !== null}
                onClick={() => void onFinish(false)}
              >
                {tr("restore_finish", undefined, "Finish")}
              </Button>
              <Button
                size="sm"
                variant="secondary"
                leftIcon={<Undo2 size={13} />}
                loading={busy === "rollback"}
                disabled={busy !== null}
                onClick={() => void onFinish(true)}
              >
                {tr("restore_rollback", undefined, "Put back")}
              </Button>
            </div>
          ) : undefined
        }
      />
    </div>
  );
}
