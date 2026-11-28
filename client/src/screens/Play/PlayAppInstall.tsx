import { useCallback, useEffect, useState } from "react";
import { Download, Play, RefreshCw, Trash2, Tv } from "lucide-react";

import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import {
  playAppErrorText,
  playAppInstallApi,
  playAppStatusText,
  type PlayAppResult,
  type PlayAppStatus,
} from "./playAppInstallState";

/** How often the card looks again while it shows (the app opened or closed on the PS5). */
const REFRESH_MS = 10000;

type Busy = "install" | "remove" | "open" | null;

/**
 * PS5Upload Play on one PS5: what is there, and Install / Update / Open / Remove with the copy
 * this PS5Upload carries (Play › Play Together, Settings › This PS5). `bare` leaves out the card's
 * own frame and title, for a place that already has one.
 */
export function PlayAppInstall({ host, bare = false }: { host: string | null; bare?: boolean }) {
  const tr = useTr();
  const { confirm, choose, dialog } = useConfirm();
  const [status, setStatus] = useState<PlayAppStatus | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [note, setNote] = useState<{ text: string; ok: boolean } | null>(null);

  const refresh = useCallback(async () => {
    if (!host) return;
    setStatus(await playAppInstallApi.status(host));
  }, [host]);

  useEffect(() => {
    setStatus(null);
    setNote(null);
    if (!host) return;
    let alive = true;
    const look = () =>
      void playAppInstallApi.status(host).then((s) => {
        if (alive) setStatus(s);
      });
    look();
    const t = window.setInterval(look, REFRESH_MS);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [host]);

  // Nothing to show for a PS5 that doesn't answer, or an engine from before this (a line under a
  // heading of its own, so the heading is not left alone).
  if (!host || !status)
    return bare ? (
      <p className="text-sm text-[var(--color-muted)]" data-testid="pai-unknown">
        {tr("pai_unknown", undefined, "Connect this PS5 to install or update PS5Upload Play on it.")}
      </p>
    ) : null;

  const after = async (r: PlayAppResult, okText: string) => {
    setNote(r.ok ? { text: okText, ok: true } : { text: playAppErrorText(tr, r.reason, r.error), ok: false });
    await refresh();
  };

  const install = async () => {
    const update = status.action === "update";
    if (status.running) {
      const go = await confirm({
        title: tr("pai_close_title", undefined, "Close PS5Upload Play?"),
        message: tr("pai_close_body", undefined, "It is open on the PS5. It closes first; its settings stay."),
        confirmLabel: update ? tr("pai_update_btn", undefined, "Update") : tr("pai_install_btn", undefined, "Install"),
      });
      if (!go) return;
    }
    setBusy("install");
    setNote(null);
    let r = await playAppInstallApi.install(host);
    if (!r.ok && r.reason === "store_managed") {
      setBusy(null);
      const go = await confirm({
        title: tr("pai_other_title", undefined, "Update PS5Upload Play from here?"),
        message: tr(
          "pai_other_body",
          undefined,
          "PS5Upload Play on this PS5 was installed with another app. Updating it here works the same way, and that app still sees it.",
        ),
        confirmLabel: tr("pai_update_btn", undefined, "Update"),
      });
      if (!go) return;
      setBusy("install");
      r = await playAppInstallApi.install(host, { confirmStore: true });
    }
    setBusy(null);
    const pending = r.json.registration_pending === true;
    const version = typeof r.json.version === "string" ? r.json.version : "";
    await after(
      r,
      pending
        ? tr("pai_done_pending", { version }, `PS5Upload Play ${version} is on the PS5. It shows on the home screen in a moment.`)
        : update
          ? tr("pai_done_update", { version }, `Updated to ${version}.`)
          : tr("pai_done_install", { version }, `PS5Upload Play ${version} is on the PS5's home screen.`),
    );
  };

  const remove = async () => {
    const choice = await choose({
      title: tr("pai_remove_title", undefined, "Remove PS5Upload Play from this PS5?"),
      message: status.otherInstaller
        ? tr("pai_remove_other", undefined, "It was installed with another app. It goes from the home screen and its folder is deleted.")
        : tr("pai_remove_body", undefined, "It goes from the home screen and its folder is deleted."),
      destructive: true,
      confirmLabel: tr("pai_remove_btn", undefined, "Remove"),
      choices: [
        { value: "keep", label: tr("pai_remove_keep", undefined, "Keep its settings on the PS5") },
        {
          value: "data",
          label: tr("pai_remove_data", undefined, "Also delete its settings and data"),
          detail: tr("pai_remove_data_detail", undefined, "Paired computers and choices made in the app are forgotten."),
        },
      ],
    });
    if (!choice) return;
    setBusy("remove");
    setNote(null);
    const r = await playAppInstallApi.remove(host, { deleteData: choice === "data", confirmStore: true });
    setBusy(null);
    await after(r, tr("pai_done_remove", undefined, "PS5Upload Play was removed from this PS5."));
  };

  const open = async () => {
    setBusy("open");
    setNote(null);
    const r = await playAppInstallApi.open(host);
    setBusy(null);
    await after(r, tr("pai_done_open", undefined, "PS5Upload Play is starting on the PS5."));
  };

  const body = (
    <>
      <p className="text-sm text-[var(--color-muted)]" data-testid="pai-status" data-action={status.action}>
        {playAppStatusText(tr, status)}
      </p>
      {status.installed && status.installed.registered === false && (
        <p className="text-xs text-[var(--color-warn)]" data-testid="pai-not-listed">
          {tr("pai_not_listed", undefined, "Its folder is there, but it isn't on the home screen yet.")}
        </p>
      )}
      {status.otherInstaller && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="pai-other">
          {tr("pai_other_note", undefined, "Installed with another app. PS5Upload can update and remove it too.")}
        </p>
      )}
      {status.storeBusy && (
        <p className="text-xs text-[var(--color-warn)]" data-testid="pai-store-busy">
          {playAppErrorText(tr, "store_busy", null)}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {(status.action === "install" || status.action === "update") && (
          <Button
            size="sm"
            variant="primary"
            leftIcon={status.action === "update" ? <RefreshCw size={14} /> : <Download size={14} />}
            loading={busy === "install"}
            disabled={busy !== null || status.storeBusy}
            onClick={() => void install()}
            data-testid="pai-install"
          >
            {status.action === "update"
              ? tr("pai_update_to", { version: status.bundled?.version ?? "" }, `Update to ${status.bundled?.version ?? ""}`)
              : tr("pai_install_btn", undefined, "Install")}
          </Button>
        )}
        {status.installed && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Play size={14} />}
            loading={busy === "open"}
            disabled={busy !== null}
            onClick={() => void open()}
            data-testid="pai-open"
          >
            {status.running ? tr("pai_open_again", undefined, "Bring to front") : tr("pai_open", undefined, "Open on the PS5")}
          </Button>
        )}
        {status.installed && (
          <Button
            size="sm"
            variant="ghost"
            leftIcon={<Trash2 size={14} />}
            loading={busy === "remove"}
            disabled={busy !== null || status.storeBusy}
            onClick={() => void remove()}
            data-testid="pai-remove"
          >
            {tr("pai_remove_btn", undefined, "Remove")}
          </Button>
        )}
      </div>
      {note && (
        <p
          role="status"
          className={`text-sm ${note.ok ? "text-[var(--color-success)]" : "text-[var(--color-danger)]"}`}
          data-testid="pai-note"
        >
          {note.text}
        </p>
      )}
      {dialog}
    </>
  );

  if (bare) return <div className="space-y-2" data-testid="pai-card">{body}</div>;
  return (
    <section
      aria-labelledby="pai-title"
      className="space-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
      data-testid="pai-card"
    >
      <h2 id="pai-title" className="flex items-center gap-2 text-sm font-semibold">
        <Tv size={16} aria-hidden />
        {tr("pai_title", undefined, "PS5Upload Play on this PS5")}
      </h2>
      {body}
    </section>
  );
}
