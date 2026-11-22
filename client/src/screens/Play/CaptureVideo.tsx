// The capture device's picture over the Remote Play picture (#893, state/capturePicture.ts):
// laid out the same way (the place's padding, the picture size setting, full screen) and, like
// it, never taking a press, so the stage's controls, the pointer and the overlays work as before.

import { useEffect, useRef } from "react";

import { canvasClass, type PictureFit } from "../../lib/remotePlayStream";
import { setShownCaptureVideo, useCaptureStore } from "../../state/capturePicture";

export function CaptureVideo({
  host,
  fit,
  placeStyle,
}: {
  host: string | null;
  fit: PictureFit;
  /** The picture place's own style (where the Remote Play picture sits). */
  placeStyle: React.CSSProperties | undefined;
}) {
  const media = useCaptureStore((s) => (s.host === host ? s.media : null));
  const shape = useCaptureStore((s) => s.shape);
  const video = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    const v = video.current;
    if (!v || !media) return;
    v.srcObject = media;
    void v.play().catch(() => {});
    setShownCaptureVideo(v);
    // The shape as the device really sends it (its settings can say another).
    const onSize = () => {
      if (v.videoWidth && v.videoHeight) useCaptureStore.setState({ shape: v.videoWidth / v.videoHeight });
    };
    v.addEventListener("loadedmetadata", onSize);
    v.addEventListener("resize", onSize);
    return () => {
      v.removeEventListener("loadedmetadata", onSize);
      v.removeEventListener("resize", onSize);
      setShownCaptureVideo(null);
      v.srcObject = null;
    };
  }, [media]);

  if (!media) return null;
  return (
    <div
      className="rp-place pointer-events-none absolute inset-0 z-0 flex items-center justify-center bg-black"
      style={placeStyle}
      data-testid="rp-capture-picture"
    >
      <video
        ref={video}
        // Its sound, when used, plays from state/capturePicture.ts (one player for all screens).
        muted
        autoPlay
        playsInline
        disablePictureInPicture
        className={canvasClass(fit)}
        data-fit={fit}
        style={{ "--rp-shape": String(shape ?? 16 / 9) } as React.CSSProperties}
      />
    </div>
  );
}
