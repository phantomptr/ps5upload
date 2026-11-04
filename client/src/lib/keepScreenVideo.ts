// Keeping a phone's screen on where the Screen Wake Lock API is missing: it needs a secure
// context, and the web UI, /join and the pad page are usually plain http on the LAN. A phone
// browser keeps the screen on while a video plays, so a tiny looping one plays out of sight
// (the technique NoSleep.js made known). The file has a silent audio track on purpose: Chromium
// only holds the screen for a video with audio. A browser may refuse to start it without a user
// gesture; the next touch starts it then.

/** 4 s, 16x16, black, silent; H.264 baseline + AAC, which every phone browser plays:
 *  ffmpeg -f lavfi -i color=black:s=16x16:r=1 -f lavfi -i anullsrc=r=8000:cl=mono -t 4
 *    -c:v libx264 -profile:v baseline -preset veryslow -crf 51 -pix_fmt yuv420p -g 4
 *    -c:a aac -b:a 8k -movflags +faststart nosleep.mp4 */
const CLIP = "data:video/mp4;base64,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";

let video: HTMLVideoElement | null = null;
let awaitingTouch: (() => void) | null = null;

function play(v: HTMLVideoElement): void {
  v.play().catch(() => {
    if (awaitingTouch || video !== v) return;
    const retry = () => {
      stopWaiting();
      if (video === v) void v.play().catch(() => {});
    };
    window.addEventListener("pointerdown", retry, { capture: true });
    awaitingTouch = () => window.removeEventListener("pointerdown", retry, { capture: true });
  });
}

function stopWaiting(): void {
  awaitingTouch?.();
  awaitingTouch = null;
}

/** Start the video, or start it again (a browser pauses it when the page is hidden). */
export function startKeepScreenVideo(): void {
  if (typeof document === "undefined") return;
  if (!video) {
    const v = document.createElement("video");
    v.muted = true;
    v.loop = true;
    v.playsInline = true;
    v.setAttribute("playsinline", "");
    v.setAttribute("aria-hidden", "true");
    v.setAttribute("data-testid", "keep-screen-video");
    v.style.cssText = "position:fixed;left:0;top:0;width:1px;height:1px;opacity:0;pointer-events:none";
    v.src = CLIP;
    document.body.appendChild(v);
    video = v;
  }
  play(video);
}

export function stopKeepScreenVideo(): void {
  stopWaiting();
  if (!video) return;
  video.pause();
  video.removeAttribute("src");
  video.remove();
  video = null;
}
