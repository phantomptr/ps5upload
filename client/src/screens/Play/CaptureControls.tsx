import { useEffect, useMemo, useState } from "react";
import { Camera, History, Square, Video } from "lucide-react";

import { canRecordClips, clipClock } from "../../lib/rpCapture";
import { useTr } from "../../state/lang";
import { useRpCaptureStore, type CaptureNotice } from "../../state/rpCapture";
import { useRemotePlayStore } from "../../state/remotePlay";

type Tr = ReturnType<typeof useTr>;

/** Presses on these never reach the picture under them (play, full screen, the pointer). */
const keep = {
  onDoubleClick: (e: React.MouseEvent) => e.stopPropagation(),
  onPointerDown: (e: React.PointerEvent) => e.stopPropagation(),
};

function useCapture() {
  const tr = useTr();
  const recordingSince = useRpCaptureStore((s) => s.recordingSince);
  const saving = useRpCaptureStore((s) => s.saving);
  const screenshot = useRpCaptureStore((s) => s.screenshot);
  const toggleRecording = useRpCaptureStore((s) => s.toggleRecording);
  const saveReplay = useRpCaptureStore((s) => s.saveReplay);
  const replaying = useRpCaptureStore((s) => s.replaying);
  const replayOn = useRemotePlayStore((s) => s.prefs.replayS > 0);
  const canRecord = useMemo(() => canRecordClips(), []);
  const recording = recordingSince !== null;
  return {
    tr,
    recording,
    saving,
    canRecord,
    replayOn,
    replaying,
    saveReplay: () => void saveReplay(),
    replayLabel: replaying
      ? tr("rp_replay_saving", undefined, "Saving the replay…")
      : tr("rp_replay_save", undefined, "Save replay"),
    screenshot: () => void screenshot(),
    toggleRecording,
    shotLabel: tr("rp_screenshot", undefined, "Screenshot"),
    recordLabel: recording
      ? tr("rp_record_stop", undefined, "Stop recording")
      : tr("rp_record", undefined, "Record a clip"),
  };
}

/** Screenshot and Record on the picture's bar (the desktop player, on the page and in full
 *  screen). Styled as the bar's other round buttons. */
export function CaptureBarButtons() {
  const c = useCapture();
  const round = (on: boolean, rec = false) =>
    `flex h-9 w-9 items-center justify-center rounded-full text-white transition-colors hover:bg-white/20 ${
      rec ? "bg-[var(--color-danger,#dc2626)]" : on ? "bg-white/25" : "bg-black/40"
    }`;
  return (
    <>
      <button
        type="button"
        aria-label={c.shotLabel}
        title={c.shotLabel}
        onClick={(e) => {
          e.stopPropagation();
          c.screenshot();
        }}
        {...keep}
        className={round(false)}
        data-testid="rp-screenshot"
      >
        <Camera size={16} />
      </button>
      {c.canRecord && (
        <button
          type="button"
          aria-label={c.recordLabel}
          title={c.recordLabel}
          aria-pressed={c.recording}
          disabled={c.saving}
          onClick={(e) => {
            e.stopPropagation();
            c.toggleRecording();
          }}
          {...keep}
          className={`${round(false, c.recording)} disabled:opacity-50`}
          data-testid="rp-record"
        >
          {c.recording ? <Square size={13} /> : <Video size={16} />}
        </button>
      )}
      {c.replayOn && (
        <button
          type="button"
          aria-label={c.replayLabel}
          title={c.replayLabel}
          disabled={c.replaying}
          onClick={(e) => {
            e.stopPropagation();
            c.saveReplay();
          }}
          {...keep}
          className={`${round(false)} disabled:opacity-50`}
          data-testid="rp-replay-save"
        >
          <History size={16} />
        </button>
      )}
    </>
  );
}

/** The same two for the phone player's menu: a slim row of their own under the picture, or
 *  (`icons`) round buttons in the full-screen menu's title row, which takes no height from a
 *  phone held sideways. */
export function CaptureRow({ onDone, icons }: { onDone?: () => void; icons?: boolean }) {
  const c = useCapture();
  if (icons) {
    const round = (rec: boolean) =>
      `flex h-9 w-9 shrink-0 items-center justify-center rounded-full transition-colors disabled:opacity-50 ${
        rec ? "bg-[var(--color-danger,#dc2626)]" : "bg-white/10 hover:bg-white/20"
      }`;
    return (
      <>
        <button
          type="button"
          aria-label={c.shotLabel}
          onClick={(e) => {
            e.stopPropagation();
            onDone?.();
            c.screenshot();
          }}
          {...keep}
          className={round(false)}
          data-testid="rp-screenshot"
        >
          <Camera size={16} />
        </button>
        {c.canRecord && (
          <button
            type="button"
            aria-label={c.recordLabel}
            aria-pressed={c.recording}
            disabled={c.saving}
            onClick={(e) => {
              e.stopPropagation();
              onDone?.();
              c.toggleRecording();
            }}
            {...keep}
            className={round(c.recording)}
            data-testid="rp-record"
          >
            {c.recording ? <Square size={13} /> : <Video size={16} />}
          </button>
        )}
        {c.replayOn && (
          <button
            type="button"
            aria-label={c.replayLabel}
            disabled={c.replaying}
            onClick={(e) => {
              e.stopPropagation();
              onDone?.();
              c.saveReplay();
            }}
            {...keep}
            className={round(false)}
            data-testid="rp-replay-save"
          >
            <History size={16} />
          </button>
        )}
      </>
    );
  }
  const pill = (rec: boolean) =>
    `flex h-9 min-w-0 flex-1 items-center justify-center gap-1.5 rounded-lg px-2 text-xs font-medium text-white transition-colors disabled:opacity-50 ${
      rec ? "bg-[var(--color-danger,#dc2626)] hover:brightness-110" : "hover:bg-white/15"
    }`;
  return (
    <div className="flex items-center gap-1 rounded-xl bg-white/10 p-1" data-testid="rp-menu-capture">
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          // The menu goes first, out of the way of what comes next.
          onDone?.();
          c.screenshot();
        }}
        {...keep}
        className={pill(false)}
        data-testid="rp-screenshot"
      >
        <Camera size={16} className="shrink-0" />
        <span className="truncate">{c.shotLabel}</span>
      </button>
      {c.canRecord && (
        <button
          type="button"
          aria-pressed={c.recording}
          disabled={c.saving}
          onClick={(e) => {
            e.stopPropagation();
            onDone?.();
            c.toggleRecording();
          }}
          {...keep}
          className={pill(c.recording)}
          data-testid="rp-record"
        >
          {c.recording ? <Square size={14} className="shrink-0" /> : <Video size={16} className="shrink-0" />}
          <span className="truncate">{c.recordLabel}</span>
        </button>
      )}
    </div>
  );
}

function noticeText(tr: Tr, n: CaptureNotice): string {
  if (n.kind === "error") {
    return n.what === "shot"
      ? tr("rp_shot_failed", undefined, "Couldn't save the screenshot.")
      : tr("rp_clip_failed", undefined, "Couldn't record or save the clip.");
  }
  const where = n.path ?? n.name;
  return n.kind === "shot"
    ? tr("rp_shot_saved", { where }, `Screenshot saved: ${where}`)
    : tr("rp_clip_saved", { where }, `Clip saved: ${where}`);
}

/** Over the picture: the recording light with its time (a press stops it), and what the last
 *  screenshot or clip came to. */
export function CaptureOverlay({ top }: { top?: string }) {
  const tr = useTr();
  const recordingSince = useRpCaptureStore((s) => s.recordingSince);
  const saving = useRpCaptureStore((s) => s.saving);
  const notice = useRpCaptureStore((s) => s.notice);
  const stop = useRpCaptureStore((s) => s.stopRecording);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (recordingSince === null) return;
    const t = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(t);
  }, [recordingSince]);
  const clock = recordingSince === null ? "" : clipClock(now - recordingSince);
  return (
    <>
      {(recordingSince !== null || saving) && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            stop();
          }}
          {...keep}
          disabled={saving}
          aria-label={tr("rp_record_stop", undefined, "Stop recording")}
          className="absolute end-3 z-30 flex items-center gap-1.5 rounded-full bg-black/60 px-2.5 py-1 font-mono text-xs text-white"
          style={{ top: top ?? "calc(max(0.75rem, env(safe-area-inset-top)) + 2.75rem)" }}
          data-testid="rp-recording"
        >
          <span
            className={`inline-block h-2.5 w-2.5 rounded-full bg-[var(--color-danger,#dc2626)] ${saving ? "" : "animate-pulse"}`}
          />
          {saving
            ? tr("rp_clip_saving", undefined, "Saving the clip…")
            : tr("rp_recording", { time: clock }, `Recording ${clock}`)}
        </button>
      )}
      {notice && (
        <div
          role="status"
          className="pointer-events-none absolute bottom-3 left-1/2 z-30 max-w-[calc(100%-2rem)] -translate-x-1/2 truncate rounded-full bg-black/70 px-3 py-1.5 text-xs text-white"
          data-testid="rp-capture-notice"
        >
          {noticeText(tr, notice)}
        </div>
      )}
    </>
  );
}
