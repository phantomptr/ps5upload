import { useEffect, useState } from "react";

import { Button, Select, Toggle } from "../../components";
import { DEFAULT_PICTURE, type Enhance, type PictureTools } from "../../lib/rpPicture";
import { hdrScreen, hdrSupport, type HdrSupport } from "../../lib/rpHdr";
import { useTr } from "../../state/lang";
import { isMobile } from "../../lib/platform";
import { appSavesToPhotos } from "../../lib/rpCapture";
import { gameModeKind } from "../../lib/gameMode";
import { keysFor, rebind, PAD_ACTIONS, type KeyAction } from "../../lib/remotePlayStream";
import { useRemotePlayStore, type RpPrefs } from "../../state/remotePlay";
import { askMotion, motionAvailable } from "../../lib/motion";
import { dualSenseCapable } from "../../lib/dualSense";
import { connectWebHidTouchpad, webHidTouchpadAvailable, webHidTouchpadConnected } from "../../lib/padTouchpad";
import { MicSetting } from "./MicControl";
import { ControllerSetup } from "./ControllerSetup";
import { ButtonRemap } from "./ButtonRemap";
import { RpNetworkSettings } from "./NetworkExtras";
import { RpBlockerSetting } from "./RpBlockers";

/** Remote Play's settings, one tab at a time (in the settings drawer, SettingsDrawer.tsx). */
export type SettingsTab = "picture" | "sound" | "controls" | "session";

/** A key code as people say it: KeyQ → Q, Digit1 → 1, ArrowUp → ↑. */
export function keyLabel(code: string): string {
  const arrows: Record<string, string> = { ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→" };
  return arrows[code] ?? code.replace(/^Key/, "").replace(/^Digit/, "").replace("Backquote", "`");
}

/** Change which key does what, saved on this device. */
function KeyRemap({
  keymap,
  onChange,
}: {
  keymap: Record<string, KeyAction>;
  onChange: (k: Record<string, KeyAction> | null) => void;
}) {
  const tr = useTr();
  const [listening, setListening] = useState<string | null>(null);

  useEffect(() => {
    if (!listening) return;
    const action = PAD_ACTIONS.find((a) => a.id === listening)?.action;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setListening(null);
      // Esc only cancels: the browser keeps it for leaving pointer lock and full screen.
      if (e.code === "Escape" || !action) return;
      onChange(rebind(keymap, action, e.code));
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [listening, keymap, onChange]);

  return (
    <div className="space-y-2">
      <div className="grid grid-cols-1 gap-y-1">
        {PAD_ACTIONS.map((a) => (
          <div key={a.id} className="flex items-center justify-between gap-2">
            <span>{/[a-z]/.test(a.label) ? tr(`rp_action_${a.id}`, undefined, a.label) : a.label}</span>
            <span className="flex items-center gap-2">
              <span className="font-mono text-[var(--color-text)]">
                {keysFor(keymap, a.action).map(keyLabel).join(", ") || "—"}
              </span>
              <Button size="sm" variant="ghost" onClick={() => setListening(a.id)}>
                {listening === a.id ? tr("rp_press_a_key", undefined, "Press a key…") : tr("rp_set_key", undefined, "Set key")}
              </Button>
            </span>
          </div>
        ))}
      </div>
      <Button size="sm" onClick={() => onChange(null)}>
        {tr("rp_keys_reset", undefined, "Reset to defaults")}
      </Button>
    </div>
  );
}

/** Settings › Picture: sharpening and upscaling, colour, a 21:9 crop and sharp pixels
 *  (lib/rpPicture.ts). They apply at once; with all of them off the picture is drawn exactly as
 *  the console sends it. */
function PictureToolsSetting() {
  const tr = useTr();
  const pic = useRemotePlayStore((s) => s.prefs.picture);
  const hdr = useRemotePlayStore((s) => s.prefs.codec === "hdr");
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const set = (p: Partial<PictureTools>) => setPrefs({ picture: { ...pic, ...p } });
  const slider = (key: "sharpen" | "brightness" | "contrast" | "saturation", label: string, min: number) => (
    <label className="flex items-center gap-3 text-sm">
      <span className="w-28 shrink-0">{label}</span>
      <input
        type="range"
        min={min}
        max={min === 0 ? 100 : 50}
        value={pic[key]}
        onChange={(e) => set({ [key]: Number(e.target.value) })}
        className="w-40 accent-[var(--color-accent)]"
        aria-label={label}
        data-testid={`rp-picture-${key}`}
      />
      <span className="w-8 text-xs tabular-nums text-[var(--color-muted)]">{pic[key]}</span>
    </label>
  );
  return (
    <fieldset className="space-y-2" data-testid="rp-picture">
      <legend className="mb-1 text-sm font-semibold">{tr("rp_picture_tools", undefined, "Picture tools")}</legend>
      <div className="space-y-1">
        <Select
          label={tr("rp_picture_enhance", undefined, "Sharpening and upscaling")}
          block={false}
          className="w-64"
          value={pic.enhance}
          onChange={(e) => set({ enhance: e.target.value as Enhance })}
          data-testid="rp-picture-enhance"
          options={[
            { value: "off", label: tr("rp_picture_enhance_off", undefined, "Off") },
            { value: "sharpen", label: tr("rp_picture_enhance_sharpen", undefined, "Sharpen (AMD FidelityFX RCAS)") },
            { value: "fsr", label: tr("rp_picture_enhance_fsr", undefined, "Upscale to the screen (AMD FSR 1)") },
          ]}
        />
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "rp_picture_enhance_hint",
            undefined,
            "Done on the graphics card, on any GPU. Sharpen makes the picture crisper. Upscale also brings it up to your screen's own resolution: best on a big or 4K screen. Off with Sharp pixels.",
          )}
        </p>
      </div>
      {pic.enhance !== "off" && slider("sharpen", tr("rp_picture_sharpen", undefined, "Sharpen"), 0)}
      {slider("brightness", tr("rp_picture_brightness", undefined, "Brightness"), -50)}
      {slider("contrast", tr("rp_picture_contrast", undefined, "Contrast"), -50)}
      {slider("saturation", tr("rp_picture_saturation", undefined, "Colour"), -50)}
      <Toggle
        checked={pic.crop219}
        onChange={(v) => set({ crop219: v })}
        label={tr("rp_picture_crop", undefined, "Crop to 21:9")}
        hint={tr(
          "rp_picture_crop_hint",
          undefined,
          "Cuts the black bars off a game made for ultrawide screens, so it fills an ultrawide screen.",
        )}
      />
      <Toggle
        checked={pic.pixelated}
        onChange={(v) => set({ pixelated: v })}
        label={tr("rp_picture_pixelated", undefined, "Sharp pixels")}
        hint={tr("rp_picture_pixelated_hint", undefined, "Scales with hard edges instead of smoothing: for retro games.")}
      />
      {hdr && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "rp_picture_hdr_note",
            undefined,
            "In HDR only Crop to 21:9 and Sharp pixels apply: the other tools work on SDR pictures.",
          )}
        </p>
      )}
      <button
        type="button"
        onClick={() => setPrefs({ picture: DEFAULT_PICTURE })}
        className="text-xs text-[var(--color-muted)] underline-offset-2 hover:underline"
        data-testid="rp-picture-reset"
      >
        {tr("rp_picture_reset", undefined, "Reset the picture")}
      </button>
    </fieldset>
  );
}

/** What the app asks of this device's OS while a stream plays (only the app can ask). */
function GameModeNote() {
  const tr = useTr();
  const kind = gameModeKind();
  if (!kind) return null;
  const text = {
    mac: tr(
      "rp_game_mode_mac",
      undefined,
      "While you play, the computer and screen stay awake. Full screen also turns on Game Mode (macOS 14 and later).",
    ),
    ios: tr(
      "rp_game_mode_ios",
      undefined,
      "Game Mode turns on while the app fills the screen (iOS and iPadOS 18 and later).",
    ),
    windows: tr(
      "rp_game_mode_windows",
      undefined,
      "While you play, the computer and screen stay awake. Windows does not let an app turn on Game Mode; see the FAQ.",
    ),
    linux: tr(
      "rp_game_mode_linux",
      undefined,
      "While you play, the computer and screen stay awake, and GameMode (gamemoded) turns on if it is installed.",
    ),
    android: tr(
      "rp_game_mode_android",
      undefined,
      "While you play, the screen stays on, the device keeps a steady speed where it can, and a TV on HDMI is asked for its game mode.",
    ),
  }[kind];
  return (
    <p className="text-xs text-[var(--color-muted)]" data-testid="rp-game-mode">
      {text}
    </p>
  );
}

/** Whether HDR can be offered here (lib/rpHdr.ts), once checked; null until then. */
function useHdrSupport(): HdrSupport | null {
  const [v, setV] = useState<HdrSupport | null>(null);
  useEffect(() => {
    let live = true;
    void hdrSupport().then((r) => live && setV(r));
    return () => {
      live = false;
    };
  }, []);
  return v;
}

/** What HDR does here, or why it is not offered (said only to someone who chose it, or who
 *  has an HDR screen and so might look for it). */
function HdrNote({ support, chosen }: { support: HdrSupport | null; chosen: boolean }) {
  const tr = useTr();
  if (!support) return null;
  let text: string | null = null;
  if (support.ok) {
    if (chosen)
      text = tr(
        "rp_codec_hdr_hint",
        undefined,
        "The PS5 sends HDR; it shows in HDR on this screen. Experimental: if the picture looks dull or too bright, go back to Auto.",
      );
  } else if (chosen || hdrScreen()) {
    text =
      support.why === "decoder"
        ? tr("rp_codec_hdr_no_decoder", undefined, "HDR is not offered: this device cannot decode the PS5's HDR video (HEVC Main10).")
        : support.why === "screen"
          ? tr("rp_codec_hdr_no_screen", undefined, "HDR is not offered: this screen is not an HDR screen, or HDR is off in the system's settings.")
          : tr("rp_codec_hdr_no_display", undefined, "HDR is not offered: this browser cannot draw HDR (it needs WebGPU with HDR, as in Chrome or Edge).");
  }
  return text ? <p className="text-xs text-[var(--color-muted)]" data-testid="rp-hdr-note">{text}</p> : null;
}

/** A group of settings in a tab, under its own small heading. */
function Section({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <section className="space-y-3">
      {title && <h3 className="text-sm font-semibold">{title}</h3>}
      {children}
    </section>
  );
}

function Hint({ children }: { children: React.ReactNode }) {
  return <p className="text-xs text-[var(--color-muted)]">{children}</p>;
}

/** One tab of Remote Play's settings. While a stream runs, what only applies on the next start
 *  says so; everything else applies at once. */
export function SettingsBody({ tab, running }: { tab: SettingsTab; running: boolean }) {
  const tr = useTr();
  const prefs = useRemotePlayStore((s) => s.prefs);
  const hdr = useHdrSupport();
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const keymap = useRemotePlayStore((s) => s.keymap);
  const setKeymap = useRemotePlayStore((s) => s.setKeymap);
  const num = (v: string) => Number(v);
  const later = running ? tr("rp_set_applies_next", undefined, "Applies when you start again") : undefined;

  if (tab === "picture") {
    return (
      <div className="space-y-6">
        <Section>
          <div className="grid grid-cols-2 gap-3">
            <Select
              label={tr("rp_quality", undefined, "Picture")}
              value={prefs.res}
              hint={later}
              onChange={(e) => setPrefs({ res: e.target.value as RpPrefs["res"] })}
              data-testid="rp-res"
              options={[
                { value: "1920x1080", label: "1080p" },
                { value: "1280x720", label: "720p" },
              ]}
            />
            <Select
              label={tr("rp_fps", undefined, "Frame rate")}
              value={String(prefs.fps)}
              hint={later}
              onChange={(e) => setPrefs({ fps: num(e.target.value) as RpPrefs["fps"] })}
              data-testid="rp-fps"
              options={[
                { value: "60", label: tr("rp_fps_60", undefined, "60 fps (smooth)") },
                { value: "30", label: tr("rp_fps_30", undefined, "30 fps (light network)") },
              ]}
            />
          </div>
          <Select
            label={tr("rp_fit", undefined, "Picture size")}
            value={prefs.fit}
            onChange={(e) => setPrefs({ fit: e.target.value as RpPrefs["fit"] })}
            data-testid="rp-fit"
            options={[
              { value: "fit", label: tr("rp_fit_fit", undefined, "Fit (whole picture)") },
              { value: "fill", label: tr("rp_fit_fill", undefined, "Fill (crop the edges)") },
              { value: "stretch", label: tr("rp_fit_stretch", undefined, "Stretch") },
            ]}
          />
          <Select
            label={tr("rp_decoding", undefined, "Video decoding")}
            value={prefs.decoding}
            hint={later}
            onChange={(e) => setPrefs({ decoding: e.target.value as RpPrefs["decoding"] })}
            data-testid="rp-decoding"
            options={[
              { value: "auto", label: tr("rp_decoding_auto", undefined, "Graphics card (auto)") },
              { value: "software", label: tr("rp_decoding_software", undefined, "Software") },
            ]}
          />
          <Select
            label={tr("rp_codec", undefined, "Video format")}
            value={prefs.codec}
            hint={later}
            onChange={(e) => setPrefs({ codec: e.target.value as RpPrefs["codec"] })}
            data-testid="rp-codec"
            options={[
              { value: "auto", label: tr("rp_codec_auto", undefined, "Auto (HEVC where it works)") },
              { value: "h264", label: "H.264" },
              // Only where it can be shown in HDR (or already chosen, to show what is set).
              ...(hdr?.ok || prefs.codec === "hdr"
                ? [{ value: "hdr", label: tr("rp_codec_hdr", undefined, "HDR (HEVC, experimental)") }]
                : []),
            ]}
          />
          <HdrNote support={hdr} chosen={prefs.codec === "hdr"} />
          <Hint>
            {tr(
              "rp_decoding_hint",
              undefined,
              "If the picture shows cut off or wrong on this device, try Software or H.264. They apply the next time you start.",
            )}
          </Hint>
          <Toggle
            checked={prefs.showStats}
            onChange={(v) => setPrefs({ showStats: v })}
            label={tr("rp_show_stats", undefined, "Show stats on the picture")}
            hint={tr("rp_show_stats_hint", undefined, "Frame rate, bitrate, decode time, loss and sound delay.")}
          />
          <RpNetworkSettings />
          <RpBlockerSetting />
        </Section>
        <PictureToolsSetting />
      </div>
    );
  }

  if (tab === "sound") {
    return (
      <div className="space-y-6">
        <Section>
          <Select
            label={tr("rp_audio_buffer", undefined, "Sound buffer")}
            value={String(prefs.audioBufferMs)}
            onChange={(e) => setPrefs({ audioBufferMs: num(e.target.value) })}
            options={[
              { value: "40", label: tr("rp_audio_low", undefined, "Lowest delay (40 ms)") },
              { value: "60", label: tr("rp_audio_normal", undefined, "Balanced (60 ms)") },
              { value: "120", label: tr("rp_audio_stable", undefined, "Steady on Wi-Fi (120 ms)") },
            ]}
          />
          <Hint>
            {tr(
              "rp_audio_hint",
              undefined,
              "Crackles or gaps on a busy network: choose Steady. On a wired network Lowest delay is best.",
            )}
          </Hint>
          <label className="flex flex-col gap-1 text-xs font-medium">
            {tr("rp_volume", undefined, "Volume")}
            <input
              type="range"
              min={0}
              max={100}
              value={Math.round(prefs.volume * 100)}
              onChange={(e) => setPrefs({ volume: num(e.target.value) / 100, muted: false })}
              className="accent-[var(--color-accent)]"
            />
          </label>
        </Section>
        <Section>
          <MicSetting />
        </Section>
      </div>
    );
  }

  if (tab === "session") {
    return (
      <div className="space-y-6">
        <Section>
          <Select
            label={tr("rp_keep_alive", undefined, "Keep playing after you leave this screen or the app")}
            value={String(prefs.keepAliveS)}
            hint={later}
            onChange={(e) => setPrefs({ keepAliveS: num(e.target.value) })}
            data-testid="rp-keep-alive"
            options={[
              { value: "30", label: tr("rp_keep_30s", undefined, "30 seconds") },
              { value: "60", label: tr("rp_keep_1m", undefined, "1 minute") },
              { value: "300", label: tr("rp_keep_5m", undefined, "5 minutes") },
              { value: "600", label: tr("rp_keep_10m", undefined, "10 minutes") },
            ]}
          />
          <Hint>
            {tr(
              "rp_keep_hint",
              undefined,
              "Other screens keep the stream running; this is how long it waits when no window shows it (closed or reloaded) before it ends.",
            )}
          </Hint>
          <GameModeNote />
        </Section>
        <Section>
          <Select
            label={tr("rp_replay_setting", undefined, "Instant replay")}
            value={String(prefs.replayS)}
            onChange={(e) => setPrefs({ replayS: Number(e.target.value) })}
            data-testid="rp-replay-setting"
            options={[
              { value: "0", label: tr("rp_replay_off", undefined, "Off") },
              ...[15, 30, 60].map((n) => ({
                value: String(n),
                label: tr("rp_replay_last", { n }, `Keep the last ${n} seconds`),
              })),
            ]}
          />
          <Hint>
            {tr(
              "rp_replay_hint",
              undefined,
              "While you play, the last seconds are kept on this computer; Save replay (on the bar) saves them as a video. Nothing is saved until you press it.",
            )}
          </Hint>
          {isMobile() && (
            <Toggle
              checked={prefs.toPhotos}
              onChange={(v) => setPrefs({ toPhotos: v })}
              label={tr("rp_to_photos", undefined, "Save screenshots and clips to Photos")}
              hint={
                appSavesToPhotos()
                  ? tr(
                      "rp_to_photos_hint_app",
                      undefined,
                      "Straight into Photos (on Android, the gallery's PS5Upload album). Off, they go to Downloads.",
                    )
                  : tr(
                      "rp_to_photos_hint_web",
                      undefined,
                      "After each one, tap Save to Photos and choose Save Image or Save Video. Off, they download.",
                    )
              }
            />
          )}
        </Section>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <Section>
        <Toggle
          checked={prefs.capturePointer}
          onChange={(v) => setPrefs({ capturePointer: v })}
          label={tr("rp_capture_pointer", undefined, "Clicking the picture hides the pointer")}
          hint={tr("rp_capture_pointer_hint", undefined, "Esc shows it again.")}
        />
        <Toggle
          checked={prefs.rumble}
          onChange={(v) => setPrefs({ rumble: v })}
          label={tr("rp_rumble", undefined, "Controller vibration")}
        />
        {dualSenseCapable() && (
          <Toggle
            checked={prefs.dualSense}
            onChange={(v) => setPrefs({ dualSense: v })}
            label={tr("rp_dualsense", undefined, "DualSense effects")}
            hint={tr(
              "rp_dualsense_hint",
              undefined,
              "With a DualSense connected to this computer: adaptive triggers, and haptics when it is plugged in with USB. Applies from the next start.",
            )}
          />
        )}
        {motionAvailable() && typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches && (
          <Toggle
            checked={prefs.motion}
            onChange={(v) => {
              if (!v) setPrefs({ motion: false });
              else void askMotion().then((ok) => ok && setPrefs({ motion: true }));
            }}
            label={tr("rp_motion", undefined, "Motion controls (experimental)")}
            hint={tr(
              "rp_motion_hint",
              undefined,
              "Tilting and turning the phone moves the controller's motion sensors, for games with gyro aiming.",
            )}
          />
        )}
        <Select
          label={tr("rp_touch_controls", undefined, "On-screen controller")}
          value={prefs.touchControls}
          onChange={(e) => setPrefs({ touchControls: e.target.value as RpPrefs["touchControls"] })}
          options={[
            { value: "auto", label: tr("rp_touch_auto", undefined, "On touch screens") },
            { value: "on", label: tr("rp_touch_on", undefined, "Always") },
            { value: "off", label: tr("rp_touch_off", undefined, "Never") },
          ]}
        />
        {prefs.touchControls !== "off" && (
          <div className="grid grid-cols-2 items-end gap-3">
            <Select
              label={tr("rp_touch_size", undefined, "Controller size")}
              value={prefs.touchSize}
              onChange={(e) => setPrefs({ touchSize: e.target.value as RpPrefs["touchSize"] })}
              options={[
                { value: "s", label: tr("rp_touch_size_s", undefined, "Small") },
                { value: "m", label: tr("rp_touch_size_m", undefined, "Medium") },
                { value: "l", label: tr("rp_touch_size_l", undefined, "Large") },
              ]}
            />
            <label className="flex flex-col gap-1 text-xs font-medium">
              {tr("rp_touch_opacity", undefined, "Controller opacity")}
              <input
                type="range"
                min={25}
                max={100}
                step={5}
                value={Math.round(prefs.touchOpacity * 100)}
                onChange={(e) => setPrefs({ touchOpacity: num(e.target.value) / 100 })}
                className="h-9 accent-[var(--color-accent)]"
                data-testid="rp-touch-opacity"
              />
            </label>
          </div>
        )}
        <Hint>
          {tr(
            "rp_controls_hint",
            undefined,
            "A controller works too: plug it in or pair it with this device. Shift + drag on the picture is a finger on the touchpad.",
          )}
        </Hint>
      </Section>

      <Section title={tr("rp_pads_title", undefined, "Controllers")}>
        <ControllerSetup />
        <WebHidTouchpad />
      </Section>

      <Section title={tr("rp_buttons_title", undefined, "Controller buttons")}>
        <ButtonRemap />
      </Section>

      <Section title={tr("rp_keys_title", undefined, "Keyboard controls")}>
        <KeyRemap keymap={keymap} onChange={setKeymap} />
      </Section>
    </div>
  );
}

/** In a browser with WebHID (Chrome, Edge): let it read a DualSense's touchpad, which the
 *  Gamepad API never gives. The browser asks which controller first, so this needs a click.
 *  The app reads the touchpad by itself and shows nothing here. */
function WebHidTouchpad() {
  const tr = useTr();
  const [on, setOn] = useState(webHidTouchpadConnected);
  if (!webHidTouchpadAvailable()) return null;
  if (on) {
    return (
      <p className="text-xs text-[var(--color-muted)]" data-testid="rp-hid-touchpad-on">
        {tr("rp_hid_touchpad_on", undefined, "The DualSense touchpad is read in this browser.")}
      </p>
    );
  }
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Button size="sm" onClick={() => void connectWebHidTouchpad().then(setOn)} data-testid="rp-hid-touchpad">
        {tr("rp_hid_touchpad", undefined, "Use the DualSense touchpad")}
      </Button>
      <span className="text-xs text-[var(--color-muted)]">
        {tr("rp_hid_touchpad_hint", undefined, "The browser asks which controller: pick the DualSense.")}
      </span>
    </div>
  );
}
