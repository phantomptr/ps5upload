// Settings › Window › Graphics: Safe graphics mode for the Linux app. WebKitGTK's rescues for a
// white or blank window (src-tauri/src/linux_gpu.rs) are environment variables read before the
// webview starts, so the switch is saved for the next launch.

import { useEffect, useState } from "react";

import { Toggle } from "../../components/Toggle";
import {
  linuxGraphicsGet,
  linuxGraphicsPossible,
  linuxGraphicsSafeModeSet,
  type GraphicsStatus,
} from "../../lib/linuxGraphics";
import { useTr } from "../../state/lang";

export function LinuxGraphicsSettings() {
  const tr = useTr();
  const [status, setStatus] = useState<GraphicsStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!linuxGraphicsPossible()) return;
    linuxGraphicsGet()
      .then(setStatus)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  if (!linuxGraphicsPossible() || !status?.linux) return null;
  const { launch } = status;

  const now =
    !launch.compositingOff && !launch.dmabufOff
      ? tr("linux_graphics_now_full", undefined, "Now: full GPU rendering.")
      : launch.safeMode
        ? tr(
            "linux_graphics_now_setting",
            undefined,
            "Now: safe graphics, as set here (or with --safe-graphics).",
          )
        : launch.setByApp
          ? tr(
              "linux_graphics_now_auto",
              undefined,
              "Now: safe graphics, chosen automatically for this computer's graphics driver or WebKitGTK.",
            )
          : tr(
              "linux_graphics_now_env",
              undefined,
              "Now: safe graphics, from WEBKIT_DISABLE_* variables in your environment.",
            );

  const set = async (on: boolean) => {
    setBusy(true);
    try {
      setStatus(await linuxGraphicsSafeModeSet(on));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3">
      <Toggle
        checked={status.saved}
        disabled={busy}
        onChange={(checked) => void set(checked)}
        label={tr("linux_safe_graphics", undefined, "Safe graphics mode")}
        hint={tr(
          "linux_safe_graphics_hint",
          undefined,
          "For a window that shows white, blank or flickers. Turns off WebKit's GPU compositing and DMA-BUF renderer: lists scroll less smoothly, and Remote Play's Sharpen and upscaling are off. Takes effect the next time the app starts.",
        )}
      />
      <p className="text-xs text-[var(--color-muted)]">{now}</p>
      {status.saved !== launch.safeMode && (
        <p className="text-xs text-[var(--color-warn)]">
          {tr(
            "linux_safe_graphics_restart",
            undefined,
            "Restart PS5Upload to apply.",
          )}
        </p>
      )}
      {error && <p className="text-xs text-[var(--color-bad)]">{error}</p>}
    </div>
  );
}
