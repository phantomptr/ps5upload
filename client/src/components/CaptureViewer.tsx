import { useCallback, useEffect, useState } from "react";
import { Camera, ChevronLeft, ChevronRight, FolderOpen, Trash2 } from "lucide-react";

import { Button } from "./Button";
import { EmptyState } from "./EmptyState";
import { Modal } from "./Modal";
import { Spinner } from "./Spinner";
import { useConfirm } from "./ConfirmDialog";
import { SCREENSHOT_EVENT, type SavedShot } from "../lib/captureScreenshot";
import {
  CAPTURES_DELETED_EVENT,
  captureDate,
  deleteAppCaptures,
  indexAfterDelete,
  listCaptures,
  openCapturesFolder,
  stepIndex,
  useCaptureViewer,
  withoutNames,
} from "../lib/appCaptures";
import { formatBytes } from "../lib/format";
import { useFormatDate } from "../lib/formatDate";
import { isMobile } from "../lib/platform";
import { useLangStore, useTr } from "../state/lang";
import { humanizeCaught } from "../lib/humanizeError";

/**
 * The app's captures, large: one at a time with previous/next (buttons, the
 * arrow keys, or the strip of thumbnails below), and Delete / Show in folder.
 * Mounted once in the shell; opened from the status bar and the Bug report.
 */
export function CaptureViewer() {
  const { open, start, close } = useCaptureViewer();
  const tr = useTr();
  const fmtDate = useFormatDate();
  const rtl = useLangStore((s) => s.lang === "ar" || s.lang === "fa");
  const { confirm, dialog } = useConfirm();
  const [shots, setShots] = useState<SavedShot[] | null>(null);
  const [index, setIndex] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Read the list each time the viewer opens, and start on the capture asked for.
  useEffect(() => {
    if (!open) return;
    let live = true;
    setShots(null);
    setError(null);
    listCaptures()
      .then((list) => {
        if (!live) return;
        setShots(list);
        const at = start ? list.findIndex((s) => s.name === start) : 0;
        setIndex(at < 0 ? 0 : at);
      })
      .catch((e) => live && (setShots([]), setError(humanizeCaught(e))));
    return () => {
      live = false;
    };
  }, [open, start]);

  // A capture taken or deleted elsewhere while the viewer is open.
  useEffect(() => {
    if (!open) return;
    const onSaved = (e: Event) => {
      const shot = (e as CustomEvent<SavedShot>).detail;
      setShots((list) => (list ? [shot, ...list.filter((s) => s.name !== shot.name)] : list));
    };
    const onDeleted = (e: Event) => {
      const gone = (e as CustomEvent<string[]>).detail;
      setShots((list) => (list ? withoutNames(list, gone) : list));
    };
    window.addEventListener(SCREENSHOT_EVENT, onSaved);
    window.addEventListener(CAPTURES_DELETED_EVENT, onDeleted);
    return () => {
      window.removeEventListener(SCREENSHOT_EVENT, onSaved);
      window.removeEventListener(CAPTURES_DELETED_EVENT, onDeleted);
    };
  }, [open]);

  const count = shots?.length ?? 0;
  const current = shots && count > 0 ? shots[Math.min(index, count - 1)] : null;
  const step = useCallback((delta: number) => setIndex((i) => stepIndex(i, delta, count)), [count]);

  // Arrow keys move through the captures; in a right-to-left language the
  // left arrow is "next", as the buttons are mirrored.
  useEffect(() => {
    if (!open || count < 2) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      e.preventDefault();
      const forward = e.key === "ArrowRight" ? !rtl : rtl;
      step(forward ? 1 : -1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, count, rtl, step]);

  const remove = async () => {
    if (!current) return;
    const ok = await confirm({
      title: tr("cap_delete_confirm_title", undefined, "Delete this capture?"),
      message: tr("cap_delete_confirm_body", undefined, "It is removed from this computer. This can't be undone."),
      confirmLabel: tr("delete", undefined, "Delete"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      const gone = await deleteAppCaptures([current.name]);
      if (!gone.includes(current.name)) throw new Error(tr("cap_delete_failed", undefined, "The capture could not be deleted."));
      setIndex((i) => indexAfterDelete(i, count - 1));
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(false);
    }
  };

  const when = current ? captureDate(current.name) : null;
  const title = current
    ? `${when ? fmtDate(when) : current.name} · ${tr("cap_position", { n: index + 1, total: count }, `${index + 1} of ${count}`)}`
    : tr("cap_view_title", undefined, "Captures");

  return (
    <>
      <Modal
        open={open}
        onClose={close}
        size="full"
        title={title}
        titleIcon={<Camera size={16} />}
        panelClassName="md:max-w-5xl"
        footer={
          current ? (
            <div className="flex w-full flex-wrap items-center gap-2">
              <span className="me-auto truncate text-xs text-[var(--color-muted)]" title={current.path}>
                {current.name} · {formatBytes(current.bytes)}
              </span>
              {!isMobile() && (
                <Button
                  variant="ghost"
                  size="sm"
                  leftIcon={<FolderOpen size={14} />}
                  onClick={() => void openCapturesFolder().catch((e) => setError(String(e)))}
                >
                  {tr("cap_show_folder", undefined, "Show in folder")}
                </Button>
              )}
              <Button variant="danger" size="sm" leftIcon={<Trash2 size={14} />} onClick={() => void remove()} disabled={busy}>
                {tr("delete", undefined, "Delete")}
              </Button>
            </div>
          ) : undefined
        }
      >
        {shots === null ? (
          <div className="flex min-h-48 items-center justify-center">
            <Spinner size={20} />
          </div>
        ) : !current ? (
          <EmptyState
            icon={Camera}
            title={tr("cap_empty_title", undefined, "No captures yet")}
            message={tr(
              "cap_empty_body",
              undefined,
              "Use Capture in the status bar, bottom right, to take a picture of any screen.",
            )}
          />
        ) : (
          <div className="grid gap-3" data-testid="capture-viewer">
            {error && (
              <p role="alert" className="text-xs text-[var(--color-bad)]">
                {error}
              </p>
            )}
            <div className="relative flex items-center justify-center rounded-[var(--radius-card)] bg-[var(--color-surface)] p-2">
              <img
                src={current.data_url}
                alt={when ? fmtDate(when) : current.name}
                className="max-h-[60vh] w-auto max-w-full rounded-[var(--radius-field)] object-contain shadow-[var(--shadow-2)]"
              />
              {count > 1 && (
                <>
                  <button
                    type="button"
                    onClick={() => step(-1)}
                    aria-label={tr("cap_prev", undefined, "Previous capture")}
                    className="icon-orb-button absolute start-3 top-1/2 -translate-y-1/2"
                  >
                    <ChevronLeft size={18} className="rtl:rotate-180" />
                  </button>
                  <button
                    type="button"
                    onClick={() => step(1)}
                    aria-label={tr("cap_next", undefined, "Next capture")}
                    className="icon-orb-button absolute end-3 top-1/2 -translate-y-1/2"
                  >
                    <ChevronRight size={18} className="rtl:rotate-180" />
                  </button>
                </>
              )}
            </div>
            {count > 1 && (
              <ul className="flex gap-2 overflow-x-auto pb-1" aria-label={tr("cap_view_title", undefined, "Captures")}>
                {shots.map((s, i) => {
                  const on = i === index;
                  const d = captureDate(s.name);
                  return (
                    <li key={s.name} className="shrink-0">
                      <button
                        type="button"
                        aria-current={on ? "true" : undefined}
                        aria-label={d ? fmtDate(d) : s.name}
                        onClick={() => setIndex(i)}
                        className={
                          "block rounded-[var(--radius-field)] border-2 p-0.5 transition-colors " +
                          (on
                            ? "border-[var(--color-accent)]"
                            : "border-transparent opacity-70 hover:border-[var(--color-border-strong)] hover:opacity-100")
                        }
                      >
                        <img src={s.data_url} alt="" className="aspect-video w-24 rounded-md object-cover" />
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        )}
      </Modal>
      {dialog}
    </>
  );
}
