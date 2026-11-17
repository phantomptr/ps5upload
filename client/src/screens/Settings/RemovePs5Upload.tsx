import { useState } from "react";
import { Trash2 } from "lucide-react";

import { Button, Toggle } from "../../components";
import { useConfirm, type ConfirmOptions } from "../../components/ConfirmDialog";
import { pmDelete, pmOverview, pmSetAutoload } from "../../api/payloadManager";
import { fsDelete, helperUninstall, type HelperUninstallResult } from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { disableHelperAtBoot, loadBootRecord, saveBootRecord } from "../../lib/helperAtBoot";
import { findStoredCopies, type StoredCopy } from "../../lib/helperStoredCopies";
import { transferScreenBusy } from "../../lib/ps5Transfers";
import { useConnectionStore } from "../../state/connection";
import { markHelperStopped } from "../../state/helperStopped";
import { clearHelperSend } from "../../state/helperSend";
import { liveCopyDeps } from "../../state/helperStoredCopiesRuntime";
import { useTr, type Translator } from "../../state/lang";
import { log } from "../../state/logs";
import { useUploadQueueStore } from "../../state/uploadQueue";

/**
 * "Remove PS5Upload from this PS5": for someone done with PS5Upload on a console (a Minion tile
 * that kept coming back after it was deleted on the PS5 was the report).
 *
 * 1. The helper is asked whether it would go ahead (`check`: nothing mounted in its folders);
 *    nothing changes yet.
 * 2. The copies of the helper that loaders start at boot are taken away: out of Payload
 *    Manager's boot list and deleted there, and deleted from autoloader / etaHEN folders (through
 *    the helper, so before it goes). Otherwise the next boot would start it again.
 * 3. The helper removes Minion and its tile, shows the icons it hid, exits and deletes its own
 *    folders on the console (node.uninstall).
 * 4. This app leaves the console alone until the helper is sent again (state/helperStopped).
 */

export type RemoveOutcome =
  | { kind: "cancelled" }
  | { kind: "removed"; result: Extract<HelperUninstallResult, { state: "removed" }>; copies: number }
  | { kind: "refused"; message: string };

export interface RemoveDeps {
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  busy: (host: string) => boolean;
  findCopies: (host: string) => Promise<StoredCopy[]>;
  /** Take the helper out of Payload Manager's boot list (throws when Payload Manager is not
   *  running). */
  disableAtBoot: (host: string) => Promise<void>;
  pmDelete: (host: string, filename: string) => Promise<void>;
  fsDelete: (host: string, path: string) => Promise<void>;
  uninstall: (host: string, keepBackups: boolean, check: boolean) => Promise<HelperUninstallResult>;
  onRemoved: (host: string) => void;
}

function refusedMessage(r: Extract<HelperUninstallResult, { state: "refused" }>, tr: Translator): string {
  switch (r.reason) {
    case "busy":
      return tr(
        "helper_stop_busy",
        undefined,
        "The helper is in use. Wait for the transfers and installs on this console to finish, then stop it.",
      );
    // Another stop or removal of this console's helper is running.
    case "stopping":
      return tr(
        "helper_stop_already_running",
        undefined,
        "A stop is already running on this console. Wait for it to finish, then try again.",
      );
    case "in_use":
      return tr(
        "remove_ps5u_in_use",
        { paths: r.paths.join(", ") },
        "Something is still mounted in PS5Upload's folders on the PS5. Unmount it first, then try again: {paths}",
      );
    // The PS5 itself is running something the removal would cut off (the helper says which).
    case "console_installing":
      return tr(
        "remove_ps5u_console_installing",
        undefined,
        "The PS5 is installing a package. Wait for it to finish, then try again.",
      );
    case "console_converting":
      return tr(
        "remove_ps5u_console_converting",
        undefined,
        "Convert is working on a game on the PS5. Wait for it to finish, then try again.",
      );
    case "console_extracting":
      return tr(
        "remove_ps5u_console_extracting",
        undefined,
        "The PS5 is extracting an archive. Wait for it to finish, then try again.",
      );
    case "console_busy":
      return tr(
        "remove_ps5u_console_busy",
        undefined,
        "The PS5 is busy with something PS5Upload started. Wait for it to finish, then try again.",
      );
    case "old_helper":
      return tr(
        "remove_ps5u_old_helper",
        undefined,
        "The helper running on this PS5 is too old to remove itself. Send this app's helper from the Connection screen, then try again.",
      );
    case "not_running":
      return tr(
        "remove_ps5u_not_running",
        undefined,
        "The helper isn't running on this PS5. Send it from the Connection screen, then try again.",
      );
    default:
      return tr("remove_ps5u_failed", { error: r.error }, "Couldn't remove PS5Upload: {error}");
  }
}

const fileName = (path: string) => path.slice(path.lastIndexOf("/") + 1);

/** The whole action, its parts passed in so it can be tested without a console. */
export async function removePs5UploadFlow(
  host: string,
  keepBackups: boolean,
  deps: RemoveDeps,
  tr: Translator,
): Promise<RemoveOutcome> {
  const target = host.trim();
  if (!target) return { kind: "cancelled" };
  const fail = (e: unknown) => ({
    kind: "refused" as const,
    message: tr(
      "remove_ps5u_failed",
      { error: e instanceof Error ? e.message : String(e) },
      "Couldn't remove PS5Upload: {error}",
    ),
  });
  if (deps.busy(target)) return refusedAnswer({ state: "refused", reason: "busy", error: "", paths: [] }, tr);

  // 1. Would the helper go ahead? Asked before anything is deleted.
  let check: HelperUninstallResult;
  try {
    check = await deps.uninstall(target, keepBackups, true);
  } catch (e) {
    return fail(e);
  }
  if (check.state === "refused") return refusedAnswer(check, tr);

  const copies = await deps.findCopies(target).catch(() => [] as StoredCopy[]);
  const details = copies.map((c) => c.path);
  const ok = await deps.confirm({
    title: tr("remove_ps5u_confirm_title", undefined, "Remove PS5Upload from this PS5?"),
    message: [
      tr(
        "remove_ps5u_confirm_body",
        undefined,
        "The PS5Upload Minion tile, PS5Upload's own files on the PS5 (settings, logs, cheats, saved shares) and the helper go. Icons it hid are shown again. Games, saves and other payloads stay. To use PS5Upload on this PS5 again, send the helper from the Connection screen.",
      ),
      keepBackups
        ? ""
        : tr("remove_ps5u_confirm_backups_go", undefined, "The save backups made on this PS5 are deleted too."),
      details.length
        ? tr(
            "remove_ps5u_confirm_copies",
            undefined,
            "These copies of the helper are deleted too, so nothing starts it at boot:",
          )
        : "",
    ]
      .filter(Boolean)
      .join("\n\n"),
    details: details.length ? details : undefined,
    confirmLabel: tr("remove_ps5u_go", undefined, "Remove"),
    destructive: true,
  });
  if (!ok) return { kind: "cancelled" };
  if (deps.busy(target)) return refusedAnswer({ state: "refused", reason: "busy", error: "", paths: [] }, tr);

  // 2. The copies loaders start at boot, while the helper still runs (it deletes the others).
  if (copies.some((c) => c.place === "payload-manager")) {
    await deps.disableAtBoot(target).catch((e) => {
      log.info("connection", `remove: Payload Manager's boot list not changed on ${target}: ${String(e)}`);
    });
  }
  for (const c of copies) {
    try {
      if (c.place === "payload-manager") {
        await deps.pmDelete(target, fileName(c.path)).catch(() => deps.fsDelete(target, c.path));
      } else {
        await deps.fsDelete(target, c.path);
      }
    } catch (e) {
      return {
        kind: "refused",
        message: tr(
          "remove_ps5u_copy_failed",
          { path: c.path, error: e instanceof Error ? e.message : String(e) },
          "Couldn't delete the copy of the helper at {path}: {error}. PS5Upload itself was not removed; try again.",
        ),
      };
    }
  }

  // 3. The helper takes the rest away and exits.
  let r: HelperUninstallResult;
  try {
    r = await deps.uninstall(target, keepBackups, false);
  } catch (e) {
    return fail(e);
  }
  if (r.state === "refused") return refusedAnswer(r, tr);
  if (r.state !== "removed") return fail(new Error("unexpected answer"));
  deps.onRemoved(target);
  return { kind: "removed", result: r, copies: copies.length };
}

function refusedAnswer(r: Extract<HelperUninstallResult, { state: "refused" }>, tr: Translator): RemoveOutcome {
  return { kind: "refused", message: refusedMessage(r, tr) };
}

/** What the panel says after a removal. */
export function removedMessage(out: Extract<RemoveOutcome, { kind: "removed" }>, tr: Translator): string {
  const parts = [
    tr(
      "remove_ps5u_done",
      undefined,
      "PS5Upload is removed from this PS5. To use it here again, send the helper from the Connection screen.",
    ),
  ];
  if (out.result.tile === "hidden")
    parts.push(
      tr(
        "remove_ps5u_done_hidden",
        undefined,
        "The PS5 kept its record of the Minion tile, so its icon is hidden instead.",
      ),
    );
  else if (out.result.tile_failed)
    parts.push(
      tr(
        "remove_ps5u_done_tile_failed",
        undefined,
        "The Minion tile could not be removed. Delete it on the PS5 (Options, then Delete); it no longer comes back.",
      ),
    );
  if (out.result.icons_shown > 0)
    parts.push(
      tr("remove_ps5u_icons_shown", { n: out.result.icons_shown }, "{n} hidden icons are shown again."),
    );
  return parts.join(" ");
}

function liveDeps(confirm: RemoveDeps["confirm"]): RemoveDeps {
  return {
    confirm,
    busy: (h) =>
      transferScreenBusy(h) || !!useUploadQueueStore.getState().runningHosts[hostOf(h) || h],
    findCopies: (h) => findStoredCopies(h, liveCopyDeps),
    disableAtBoot: (h) =>
      disableHelperAtBoot(h, {
        pmOverview,
        setAutoload: pmSetAutoload,
        load: loadBootRecord,
        save: saveBootRecord,
        log: (line) => log.info("connection", line),
      }),
    pmDelete,
    fsDelete: (h, p) => fsDelete(h, p),
    uninstall: helperUninstall,
    onRemoved: (h) => {
      markHelperStopped(h);
      clearHelperSend(h);
      saveBootRecord(h, null);
      useConnectionStore.getState().setHostStatus(h, { payloadStatus: "down", payloadVersion: null });
      log.info("connection", `PS5Upload removed from ${h} by the user; the helper stays off until sent again`);
    },
  };
}

/** Settings › This PS5: the panel. */
export function RemovePs5Upload({ host }: { host: string }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [keepBackups, setKeepBackups] = useState(true);
  // "checking" while the helper is asked and the confirm is up; "removing" only once confirmed.
  const [phase, setPhase] = useState<"idle" | "checking" | "removing">("idle");
  const [message, setMessage] = useState<{ tone: "ok" | "warn"; text: string } | null>(null);

  async function onClick() {
    setMessage(null);
    setPhase("checking");
    const confirmThenRemove: RemoveDeps["confirm"] = async (opts) => {
      const ok = await confirm(opts);
      if (ok) setPhase("removing");
      return ok;
    };
    try {
      const out = await removePs5UploadFlow(host, keepBackups, liveDeps(confirmThenRemove), tr);
      if (out.kind === "refused") setMessage({ tone: "warn", text: out.message });
      else if (out.kind === "removed") setMessage({ tone: "ok", text: removedMessage(out, tr) });
    } finally {
      setPhase("idle");
    }
  }
  const working = phase === "removing";

  return (
    <div className="space-y-3 text-sm" data-testid="remove-ps5upload">
      {dialog}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "remove_ps5u_hint",
          undefined,
          "For when you are done with PS5Upload on this PS5. The helper takes away what it put there: the PS5Upload Minion tile, its own files, and the copies of the helper your loaders start at boot. Games, saves and other payloads stay.",
        )}
      </p>
      <Toggle
        checked={keepBackups}
        onChange={setKeepBackups}
        label={tr("remove_ps5u_keep_backups", undefined, "Keep the save backups made on this PS5")}
      />
      <Button
        variant="danger"
        size="sm"
        leftIcon={<Trash2 size={14} />}
        onClick={() => void onClick()}
        disabled={phase !== "idle" || !host.trim()}
        loading={working}
      >
        {working
          ? tr("remove_ps5u_working", undefined, "Removing…")
          : tr("remove_ps5u_button", undefined, "Remove PS5Upload from this PS5…")}
      </Button>
      {message && (
        <p
          role={message.tone === "warn" ? "alert" : "status"}
          className={`text-xs ${message.tone === "warn" ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}
        >
          {message.text}
        </p>
      )}
    </div>
  );
}
