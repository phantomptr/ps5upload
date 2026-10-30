import { create } from "zustand";

import {
  MAX_CLIP_MS,
  canShareToPhotos,
  captureFile,
  captureFileName,
  recorderType,
  saveCapture,
} from "../lib/rpCapture";
import { useRemotePlayStore } from "./remotePlay";
import { log } from "./logs";

/** What the last screenshot or clip came to, shown on the picture for a moment. */
export type CaptureNotice =
  | { kind: "shot" | "clip"; name: string; path: string | null; photos?: boolean; denied?: boolean }
  /** A phone's browser: ready for the share sheet (Save to Photos) or a download, on a tap. */
  | { kind: "ready"; what: "shot" | "clip"; file: File }
  | { kind: "error"; what: "shot" | "clip" };

interface CaptureState {
  /** When the running recording started (Date.now()), or null. */
  recordingSince: number | null;
  /** A clip is being written out. */
  saving: boolean;
  /** The instant replay is being turned into a file. */
  replaying: boolean;
  notice: CaptureNotice | null;
  screenshot: () => Promise<void>;
  /** Save the instant replay (the last seconds the engine kept) as an MP4. */
  saveReplay: () => Promise<void>;
  /** Start a recording, or stop the one running and save it. */
  toggleRecording: () => void;
  stopRecording: () => void;
  clearNotice: () => void;
  /** Hand the ready file to the share sheet (`share`) or download it. */
  saveReady: (share: boolean) => Promise<void>;
}

/** The recording in progress (not in the store: none of it is drawn). */
let rec: {
  recorder: MediaRecorder;
  chunks: Blob[];
  release: () => void;
  mime: string;
  ext: string;
  console: string;
  started: Date;
  limit: number;
} | null = null;

let noticeTimer = 0;

export const useRpCaptureStore = create<CaptureState>((set, get) => {
  const notify = (n: CaptureNotice) => {
    window.clearTimeout(noticeTimer);
    set({ notice: n });
    const ms = n.kind === "ready" ? 20_000 : n.kind === "error" || n.denied ? 6000 : 4000;
    noticeTimer = window.setTimeout(() => set({ notice: null }), ms);
  };

  /** Save as the Save to Photos setting says. A phone's browser can only reach Photos through
   *  the share sheet, which needs a tap: the notice offers it. */
  const save = async (what: "shot" | "clip", blob: Blob, name: string) => {
    const toPhotos = useRemotePlayStore.getState().prefs.toPhotos;
    const file = captureFile(blob, name);
    if (toPhotos && canShareToPhotos(file)) {
      notify({ kind: "ready", what, file });
      return;
    }
    const r = await saveCapture(blob, name, toPhotos);
    notify({ kind: what, name, path: r.where, photos: r.photos, ...(r.denied ? { denied: true } : {}) });
  };

  return {
    recordingSince: null,
    saving: false,
    replaying: false,
    notice: null,

    async screenshot() {
      const { stream, target } = useRemotePlayStore.getState();
      if (!stream) return;
      const name = captureFileName(target?.name ?? "", new Date(), "png");
      try {
        const blob = await stream.snapshot();
        if (!blob) throw new Error("no picture yet");
        await save("shot", blob, name);
      } catch {
        notify({ kind: "error", what: "shot" });
      }
    },

    async saveReplay() {
      if (get().replaying) return;
      const { target, stream } = useRemotePlayStore.getState();
      set({ replaying: true });
      const name = captureFileName(target?.name ?? "", new Date(), "mp4").replace(/^PS5/, "PS5 replay");
      try {
        const { fetchReplayMp4 } = await import("../lib/rpReplay");
        const blob = await fetchReplayMp4({ codec: stream?.videoCodec || undefined });
        await save("clip", blob, name);
      } catch (e) {
        log.warn("remote-play", `replay: ${e instanceof Error ? e.message : String(e)}`);
        notify({ kind: "error", what: "clip" });
      } finally {
        set({ replaying: false });
      }
    },

    toggleRecording() {
      if (rec) return get().stopRecording();
      const { stream, target, prefs } = useRemotePlayStore.getState();
      const type = typeof MediaRecorder !== "undefined" ? recorderType((m) => MediaRecorder.isTypeSupported(m)) : null;
      const media = type ? stream?.captureMedia(prefs.fps) : null;
      if (!stream || !type || !media) {
        notify({ kind: "error", what: "clip" });
        return;
      }
      let recorder: MediaRecorder;
      try {
        recorder = new MediaRecorder(media.stream, { mimeType: type.mime, videoBitsPerSecond: 8_000_000 });
      } catch {
        media.release();
        notify({ kind: "error", what: "clip" });
        return;
      }
      const r = {
        recorder,
        chunks: [] as Blob[],
        release: media.release,
        mime: type.mime,
        ext: type.ext,
        console: target?.name ?? "",
        started: new Date(),
        limit: window.setTimeout(() => get().stopRecording(), MAX_CLIP_MS),
      };
      recorder.ondataavailable = (e) => {
        if (e.data.size) r.chunks.push(e.data);
      };
      recorder.onstop = () => {
        r.release();
        const blob = new Blob(r.chunks, { type: r.mime.split(";")[0] });
        const name = captureFileName(r.console, r.started, r.ext);
        if (!blob.size) {
          set({ saving: false });
          notify({ kind: "error", what: "clip" });
          return;
        }
        save("clip", blob, name)
          .catch(() => notify({ kind: "error", what: "clip" }))
          .finally(() => set({ saving: false }));
      };
      try {
        // A piece every second: a long clip never waits on one huge buffer at the end.
        recorder.start(1000);
      } catch {
        window.clearTimeout(r.limit);
        media.release();
        notify({ kind: "error", what: "clip" });
        return;
      }
      rec = r;
      set({ recordingSince: Date.now() });
    },

    stopRecording() {
      const r = rec;
      if (!r) return;
      rec = null;
      window.clearTimeout(r.limit);
      set({ recordingSince: null, saving: true });
      try {
        r.recorder.stop();
      } catch {
        r.release();
        set({ saving: false });
      }
    },

    clearNotice() {
      window.clearTimeout(noticeTimer);
      set({ notice: null });
    },

    async saveReady(share) {
      const n = get().notice;
      if (n?.kind !== "ready") return;
      const { file, what } = n;
      if (share) {
        try {
          await navigator.share({ files: [file] });
          get().clearNotice();
          return;
        } catch (e) {
          // Closed without choosing: the notice stays for another try.
          if (e instanceof DOMException && e.name === "AbortError") return;
          log.warn("remote-play", `share: ${e instanceof Error ? e.message : String(e)}`);
        }
      }
      try {
        const path = await saveCapture(file, file.name);
        notify({ kind: what, name: file.name, path: path.where });
      } catch {
        notify({ kind: "error", what });
      }
    },
  };
});

// The stream ended or another one started: what was recorded so far is saved.
useRemotePlayStore.subscribe((s, prev) => {
  if (prev.stream && s.stream !== prev.stream) useRpCaptureStore.getState().stopRecording();
});
