// Making a video decoder safely in the Linux app (issue #610).
//
// WebKitGTK 2.54 hands every WebCodecs decoder the page's GL display, and aborts the whole web
// process (the app's window dies) when the page has none yet. With the DMA-BUF renderer off, as
// the app runs on most AMD and Intel machines (src-tauri/src/linux_gpu.rs), nothing makes one
// until a WebGL canvas exists, so the first VideoDecoder.isConfigSupported() when Play is
// pressed killed the app. Reproduced on Fedora 44 (WebKitGTK 2.54.2) with no graphics card at
// all; one WebGL context first and the same check answers and decodes.
//
// As a net for any other crash there, the page also leaves a marker in the app's config folder
// while it asks WebKit for a decoder (until the first picture or the end of the stream). The
// next launch finds a marker left by a crash, decodes in software from then on and says so once
// (announceVideoRecovery).

import { invoke } from "./invokeLogged";
import { linuxGraphicsPossible } from "./linuxGraphics";
import { trStatic } from "./trStatic";
import { pushNotification } from "../state/notifications";
import { useToastStore } from "../state/toasts";

let primed = false;

/** Give the page its GL display before WebKitGTK makes a decoder: one WebGL context does it, for
 *  the life of the page. Only in the Linux app; never throws. */
export function primeDecoderDisplay(): void {
  if (primed || !linuxGraphicsPossible() || typeof document === "undefined")
    return;
  primed = true;
  try {
    const gl = document.createElement("canvas").getContext("webgl");
    // The display stays; the context itself is not needed.
    gl?.getExtension("WEBGL_lose_context")?.loseContext();
  } catch {
    // No WebGL here: nothing more to do.
  }
}

let open = 0;
let writes: Promise<void> = Promise.resolve();

function mark(on: boolean): Promise<void> {
  writes = writes.then(() =>
    invoke<void>("linux_video_starting", { on }).catch(() => undefined),
  );
  return writes;
}

export interface DecoderStart {
  /** The marker is written: the decoder may be asked for now. */
  ready: Promise<void>;
  /** A picture came, or the attempt ended. Once is enough; more calls do nothing. */
  end: () => void;
}

/** Before asking WebKit for a decoder (a check or a stream's own): prime the GL display and
 *  leave the marker while any such attempt is open. Elsewhere than the Linux app, nothing. */
export function decoderStarting(): DecoderStart {
  if (!linuxGraphicsPossible())
    return { ready: Promise.resolve(), end: () => {} };
  primeDecoderDisplay();
  open++;
  const ready = open === 1 ? mark(true) : writes;
  let ended = false;
  return {
    ready,
    end: () => {
      if (ended) return;
      ended = true;
      open--;
      if (open === 0) void mark(false);
    },
  };
}

/** Run a decoder check inside decoderStarting(). */
export async function withDecoderStart<T>(fn: () => Promise<T>): Promise<T> {
  const s = decoderStarting();
  try {
    await s.ready;
    return await fn();
  } finally {
    s.end();
  }
}

/** What the Linux app decided at launch (linux_gpu.rs, the page flag). */
function launchFlag():
  { softwareVideo?: unknown; videoRecovered?: unknown } | undefined {
  if (typeof window === "undefined") return undefined;
  return (
    window as unknown as {
      __PS5UPLOAD_LINUX_GRAPHICS__?: {
        softwareVideo?: unknown;
        videoRecovered?: unknown;
      };
    }
  ).__PS5UPLOAD_LINUX_GRAPHICS__;
}

/** The Linux app decodes video without the graphics card this launch. */
export function linuxSoftwareVideo(): boolean {
  return launchFlag()?.softwareVideo === true;
}

let announced = false;

/** Once, at launch: Play crashed while starting the picture last time, so this launch decodes in
 *  software. The app clears the marker as it reads it, so the next launch says nothing. */
export function announceVideoRecovery(): boolean {
  if (announced || launchFlag()?.videoRecovered !== true) return false;
  announced = true;
  const msg = trStatic(
    "video_recovered_notice",
    "Play crashed while starting the picture last time, so the picture is now decoded without the graphics card (Settings › Window › Graphics › Hardware video decoding to change it).",
  );
  pushNotification(
    "warning",
    trStatic("video_recovered_title", "Picture now decoded in software"),
    {
      body: msg,
      link: "/settings",
    },
  );
  useToastStore
    .getState()
    .push({ message: msg, tone: "warn", duration: 15000 });
  return true;
}

/** Tests only. */
export function resetVideoDecoderGuardForTests(): void {
  primed = false;
  open = 0;
  writes = Promise.resolve();
  announced = false;
}
