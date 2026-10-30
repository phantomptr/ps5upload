/**
 * Remote Play in picture in picture: the stream in a small window that stays on top while the
 * app is in the background or minimized.
 *
 * - Desktop app: an always-on-top popup the main window opens (`window.open("")`, answered by
 *   src-tauri/src/pip_window.rs). It shares this page's JavaScript, so the one stream keeps
 *   running here and only its canvas moves into the popup: no second session, no hand-over.
 *   When that window does not open, the system's video picture in picture below instead.
 * - Browser with Document Picture-in-Picture (Chrome, Edge 116+): the same, in the browser's
 *   own always-on-top window.
 * - Other browsers with video picture in picture (Safari): the canvas as a video. Picture only:
 *   that window takes no keys, and the controller plays while this page is in front.
 *
 * In a window of its own the pad is polled on that window's animation frames and from its own
 * gamepad list (lib/remotePlayStream setFrameWindow, state/remotePlay rpGamepads), so it keeps
 * playing while the main window is hidden. Whether a controller reaches a window that is not in
 * front is the system's choice: Chromium (browser, Windows app) gives it to any visible window;
 * WebKit (macOS app, Linux app) only to the window in front.
 */
import { useEffect, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Square, Undo2, Volume2, VolumeX } from "lucide-react";

import { Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { rpInput, useRemotePlayStore, type PipKind } from "../../state/remotePlay";
import { isMobile } from "../../lib/platform";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import { log } from "../../state/logs";
import { visibleArea } from "./pipArea";
import { phaseText } from "./StreamPanel";

const PIP_WIDTH = 480;
const PIP_HEIGHT = 270;
/** Window labels and the closed event of src-tauri/src/pip_window.rs. */
const TAURI_PIP_PREFIX = "rp-pip-";
const TAURI_PIP_CLOSED = "rp-pip-closed";

interface DocumentPip {
  requestWindow(o?: { width?: number; height?: number }): Promise<Window>;
}

function documentPip(): DocumentPip | null {
  if (typeof window === "undefined") return null;
  return (window as unknown as { documentPictureInPicture?: DocumentPip }).documentPictureInPicture ?? null;
}

/** Whether this page can show the canvas as a picture-in-picture video. */
function videoPipAvailable(): boolean {
  return (
    typeof document !== "undefined" &&
    !!document.pictureInPictureEnabled &&
    typeof HTMLCanvasElement !== "undefined" &&
    "captureStream" in HTMLCanvasElement.prototype
  );
}

/** How this device can show the picture in picture, or null (phones, Firefox). */
export function pipSupport(): PipKind | null {
  if (typeof window === "undefined" || isMobile()) return null;
  if (isTauriEnv()) return "popup";
  if (documentPip()) return "document";
  if (videoPipAvailable()) return "video";
  return null;
}

/** Whether a controller plays the picture-in-picture window while another app is in front. */
export function pipPadsInBackground(kind: PipKind): boolean {
  if (kind === "document") return true;
  if (kind === "popup") return /windows/i.test(typeof navigator === "undefined" ? "" : navigator.userAgent);
  return false;
}

/** What is open, to close it again. */
let open: {
  kind: PipKind;
  win: Window | null;
  root: Root | null;
  video: HTMLVideoElement | null;
  cleanups: Array<() => void>;
} | null = null;

/** The page's look (theme, scale, language) and styles, copied into a new window's document. */
function prepareDocument(win: Window, title: string): HTMLElement {
  const doc = win.document;
  for (const a of Array.from(document.documentElement.attributes)) {
    doc.documentElement.setAttribute(a.name, a.value);
  }
  for (const sheet of Array.from(document.styleSheets)) {
    try {
      const style = doc.createElement("style");
      style.textContent = Array.from(sheet.cssRules)
        .map((r) => r.cssText)
        .join("\n");
      doc.head.appendChild(style);
    } catch {
      // A sheet whose rules cannot be read: link it instead.
      if (!sheet.href) continue;
      const link = doc.createElement("link");
      link.rel = "stylesheet";
      link.href = sheet.href;
      doc.head.appendChild(link);
    }
  }
  doc.title = title;
  doc.body.style.cssText = "margin:0;height:100vh;overflow:hidden;background:#000;";
  const host = doc.createElement("div");
  host.style.cssText = "position:fixed;inset:0;";
  doc.body.appendChild(host);
  return host;
}

/** Bring the main window to the front (it may be minimized or in the tray). */
async function focusMainWindow(): Promise<void> {
  try {
    if (isTauriEnv()) {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const w = getCurrentWindow();
      await w.unminimize();
      await w.show();
      await w.setFocus();
    } else {
      window.focus();
    }
  } catch {
    // Not allowed here: the picture is back in the page anyway.
  }
}

async function closeTauriPopups(): Promise<void> {
  try {
    const { getAllWebviewWindows } = await import("@tauri-apps/api/webviewWindow");
    for (const w of await getAllWebviewWindows()) {
      if (w.label.startsWith(TAURI_PIP_PREFIX)) await w.close();
    }
  } catch {
    // Already gone.
  }
}

/** Take the picture out of picture in picture; `closeWindow` false when the window is already
 *  going (closed from its title bar). */
export function closePip(closeWindow = true): void {
  const cur = open;
  if (!cur) return;
  open = null;
  for (const c of cur.cleanups) c();
  // The player lets go of the canvas first, then the stream screen takes it back.
  cur.root?.unmount();
  rpInput.held.clear();
  useRemotePlayStore.getState().setPip(null);
  useRemotePlayStore.getState().stream?.sendPad();
  if (cur.video) {
    if (document.pictureInPictureElement === cur.video) void document.exitPictureInPicture().catch(() => {});
    cur.video.srcObject = null;
    cur.video.remove();
  }
  if (!closeWindow) return;
  if (cur.kind === "popup") void closeTauriPopups();
  try {
    cur.win?.close();
  } catch {
    // Already closed.
  }
}

/**
 * Show the stream in picture in picture (call from a click: browsers want a gesture).
 * `onBack` brings the app back to the Remote Play screen. False when it could not open.
 */
export async function openPip(opts: { title: string; onBack: () => void }): Promise<boolean> {
  const store = useRemotePlayStore.getState();
  const stream = store.stream;
  const kind = pipSupport();
  if (!stream || !kind) return false;
  if (open) return true;

  if (kind === "video") return openVideoPip(stream.canvas);

  let win: Window | null = null;
  try {
    win =
      kind === "document"
        ? await documentPip()!.requestWindow({ width: PIP_WIDTH, height: PIP_HEIGHT })
        : window.open("", "_blank", `width=${PIP_WIDTH},height=${PIP_HEIGHT}`);
  } catch (e) {
    console.warn(`[pip] the ${kind} window did not open:`, e);
    win = null;
  }
  if (!win) {
    if (kind === "popup") console.warn("[pip] window.open gave no window");
    return fallBackToVideo(kind, stream.canvas);
  }
  let host: HTMLElement;
  try {
    host = prepareDocument(win, opts.title);
  } catch (e) {
    // Not a window this page can draw in (the desktop app did not hand it over).
    console.warn("[pip] cannot draw in the new window:", e);
    try {
      win.close();
    } catch {
      // Nothing to close.
    }
    if (kind === "popup") void closeTauriPopups();
    return fallBackToVideo(kind, stream.canvas);
  }
  const cur = { kind, win, root: createRoot(host), video: null, cleanups: [] as Array<() => void> };
  open = cur;
  const gone = () => open === cur && closePip(false);
  win.addEventListener("pagehide", gone);
  // A window can go without a word to this page (the desktop app's title bar): check now and then.
  const poll = window.setInterval(() => win.closed && gone(), 1000);
  cur.cleanups.push(() => window.clearInterval(poll));
  if (kind === "popup") {
    let unlisten: (() => void) | null = null;
    let dropped = false;
    void import("@tauri-apps/api/event")
      .then(({ listen }) => listen(TAURI_PIP_CLOSED, gone))
      .then((u) => (dropped ? safeUnlisten(u) : (unlisten = u)))
      .catch(() => {});
    cur.cleanups.push(() => {
      dropped = true;
      if (unlisten) safeUnlisten(unlisten);
    });
    void keepToVisibleArea(win, host, cur.cleanups);
  }
  store.setPip(kind, win);
  cur.root.render(
    <MiniPlayer
      win={win}
      onBack={() => {
        closePip();
        void focusMainWindow();
        opts.onBack();
      }}
    />,
  );
  watchStreamEnd(cur);
  return true;
}

/**
 * Windows (#566, #575): the desktop app's popup can lay its page out bigger than the window it
 * shows it in, so the picture, sized against the page, showed only its top-left part in every
 * picture size. Where the window says it is smaller than the page, the player is laid out in
 * the part that shows; everywhere else (sizes agree) this changes nothing. Logged, so a report
 * says when it happened.
 */
async function keepToVisibleArea(win: Window, host: HTMLElement, cleanups: Array<() => void>): Promise<void> {
  let gone = false;
  cleanups.push(() => {
    gone = true;
  });
  try {
    const { getAllWebviewWindows } = await import("@tauri-apps/api/webviewWindow");
    const n = (label: string) => Number(label.slice(TAURI_PIP_PREFIX.length)) || 0;
    const tw = (await getAllWebviewWindows())
      .filter((w) => w.label.startsWith(TAURI_PIP_PREFIX))
      .sort((a, b) => n(b.label) - n(a.label))[0];
    if (!tw || gone) return;
    let logged = false;
    const apply = async () => {
      if (gone) return;
      try {
        const size = await tw.innerSize();
        const page = { width: win.innerWidth, height: win.innerHeight };
        const area = visibleArea(size, win.devicePixelRatio, page);
        if (!area) {
          host.style.cssText = "position:fixed;inset:0;";
          return;
        }
        host.style.cssText = `position:fixed;left:0;top:0;width:${area.width}px;height:${area.height}px;`;
        if (!logged) {
          logged = true;
          log.warn(
            "remote-play",
            `pop-out page ${page.width}x${page.height} but its window shows ${area.width.toFixed(0)}x${area.height.toFixed(0)} ` +
              `(window ${size.width}x${size.height} device px, ratio ${win.devicePixelRatio}): fitting the picture to what shows`,
          );
        }
      } catch {
        // The window is going.
      }
    };
    const onResize = () => void apply();
    win.addEventListener("resize", onResize);
    cleanups.push(() => win.removeEventListener("resize", onResize));
    const unlistens = await Promise.all([tw.onResized(onResize), tw.onScaleChanged(onResize)]);
    if (gone) unlistens.forEach((u) => safeUnlisten(u));
    else cleanups.push(() => unlistens.forEach((u) => safeUnlisten(u)));
    await apply();
    // The webview may settle its size a moment after the window opens.
    for (const ms of [250, 1000]) {
      const t = window.setTimeout(onResize, ms);
      cleanups.push(() => window.clearTimeout(t));
    }
  } catch (e) {
    console.warn("[pip] could not read the window's size:", e);
  }
}

/** A window of its own did not open (the desktop app on macOS, a browser that refused it): the
 *  system's video picture in picture instead, picture only, when this page has it. */
function fallBackToVideo(kind: PipKind, canvas: HTMLCanvasElement): Promise<boolean> | false {
  if (kind === "video" || !videoPipAvailable()) return false;
  console.warn("[pip] showing the picture as a video instead");
  return openVideoPip(canvas);
}

/** The canvas as a picture-in-picture video (Safari, and the fallback). */
async function openVideoPip(canvas: HTMLCanvasElement): Promise<boolean> {
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.style.cssText = "position:fixed;right:0;bottom:0;width:2px;height:2px;opacity:0;pointer-events:none;";
  video.srcObject = canvas.captureStream(60);
  document.body.appendChild(video);
  const cur = { kind: "video" as const, win: null, root: null, video, cleanups: [] as Array<() => void> };
  open = cur;
  try {
    await video.play();
    await video.requestPictureInPicture();
  } catch {
    // Safari's own call, for a stream it will not take through the standard one.
    const v = video as HTMLVideoElement & { webkitSetPresentationMode?: (m: string) => void };
    if (!v.webkitSetPresentationMode) {
      if (open === cur) closePip(false);
      return false;
    }
    v.webkitSetPresentationMode("picture-in-picture");
  }
  const onLeave = () => open === cur && closePip(false);
  video.addEventListener("leavepictureinpicture", onLeave);
  video.addEventListener("webkitpresentationmodechanged", () => {
    const mode = (video as HTMLVideoElement & { webkitPresentationMode?: string }).webkitPresentationMode;
    if (mode === "inline") onLeave();
  });
  useRemotePlayStore.getState().setPip("video");
  watchStreamEnd(cur);
  return true;
}

/** The stream ended (Stop, the console): picture in picture closes with it. */
function watchStreamEnd(cur: NonNullable<typeof open>): void {
  const unsub = useRemotePlayStore.subscribe((s) => {
    if (!s.stream && open === cur) closePip();
  });
  cur.cleanups.push(unsub);
}

/** The picture, filling its window, with a bar on hover: sound, back to the app, Stop. */
function MiniPlayer({ win, onBack }: { win: Window; onBack: () => void }) {
  const tr = useTr();
  const stream = useRemotePlayStore((s) => s.stream);
  const phase = useRemotePlayStore((s) => s.phase);
  const target = useRemotePlayStore((s) => s.target);
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const stop = useRemotePlayStore((s) => s.stop);
  const stage = useRef<HTMLDivElement>(null);
  const canvasHost = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = canvasHost.current;
    if (!stream || !host) return;
    host.appendChild(stream.canvas);
    stage.current?.focus();
    return () => {
      if (stream.canvas.parentElement === host) host.removeChild(stream.canvas);
    };
  }, [stream]);

  useEffect(() => {
    const name = target?.name ?? "";
    win.document.title = tr("rp_pip_title", { name }, `${name} · Remote Play`);
  }, [win, target, tr]);

  const onKey = (e: React.KeyboardEvent, down: boolean) => {
    if (!useRemotePlayStore.getState().keymap[e.code]) return;
    e.preventDefault();
    if (down) rpInput.held.add(e.code);
    else rpInput.held.delete(e.code);
    stream?.sendPad();
  };
  const muted = prefs.muted || prefs.volume === 0;
  const waiting = phase !== "playing";

  return (
    <div
      ref={stage}
      tabIndex={0}
      onKeyDown={(e) => onKey(e, true)}
      onKeyUp={(e) => onKey(e, false)}
      onBlur={() => {
        rpInput.held.clear();
        stream?.sendPad();
      }}
      className="group relative h-full w-full overflow-hidden bg-black outline-none"
      data-testid="rp-pip-player"
    >
      {/* rp-place: the picture sizes itself against this box (index.css), as on the stream's own
          screen. Without it the browser picks the box (a viewport), and the Windows app's picked
          one larger than this window: the picture was cut on the right and bottom (#566). */}
      <div ref={canvasHost} className="rp-place pointer-events-none absolute inset-0 z-0 flex items-center justify-center" />
      {waiting && (
        <div className="absolute inset-0 flex items-center justify-center gap-2 bg-black/40 text-sm text-white/85">
          <Spinner size={16} tone="inherit" />
          {phase === "moved"
            ? tr("rp_moved", undefined, "Playing in another window.")
            : phase
              ? phaseText(tr, phase)
              : null}
        </div>
      )}
      <div
        className="absolute inset-x-0 bottom-0 z-10 flex items-center gap-2 bg-gradient-to-t from-black/75 to-transparent px-2 pb-2 pt-6 opacity-0 transition-opacity duration-200 focus-within:opacity-100 group-hover:opacity-100"
        onKeyDown={(e) => e.stopPropagation()}
        onKeyUp={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          aria-label={muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
          title={muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
          onClick={() => setPrefs({ muted: !prefs.muted, volume: prefs.volume || 1 })}
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-black/40 text-white hover:bg-white/20"
        >
          {muted ? <VolumeX size={15} /> : <Volume2 size={15} />}
        </button>
        <input
          type="range"
          min={0}
          max={100}
          step={5}
          value={muted ? 0 : Math.round(prefs.volume * 100)}
          onChange={(e) => {
            const v = Number(e.target.value) / 100;
            setPrefs(v === 0 ? { muted: true } : { volume: v, muted: false });
          }}
          aria-label={tr("rp_volume", undefined, "Volume")}
          className="w-20 min-w-0 accent-white"
        />
        <span className="min-w-0 flex-1" />
        <button
          type="button"
          aria-label={tr("rp_pip_back", undefined, "Back to the app")}
          title={tr("rp_pip_back", undefined, "Back to the app")}
          onClick={onBack}
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-black/40 text-white hover:bg-white/20"
          data-testid="rp-pip-back"
        >
          <Undo2 size={15} />
        </button>
        <button
          type="button"
          onClick={stop}
          className="flex h-8 shrink-0 items-center gap-1 rounded-full bg-[var(--color-danger,#dc2626)] px-2.5 text-xs font-medium text-white hover:brightness-110"
          data-testid="rp-pip-stop"
        >
          <Square size={11} />
          {tr("rp_stop", undefined, "Stop")}
        </button>
      </div>
    </div>
  );
}
