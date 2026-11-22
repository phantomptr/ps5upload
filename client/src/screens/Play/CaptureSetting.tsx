// Settings › Picture: where Play's picture comes from for the selected console (#893), Remote
// Play or a capture device on this computer, and the sound with it. Desktop app only.

import { useEffect, useState } from "react";

import { Button, Select } from "../../components";
import { captureAvailable, captureChoiceFor, findDevice, type CaptureChoice, type DeviceInfo } from "../../lib/capturePicture";
import { listCaptureDevices, useCaptureStore } from "../../state/capturePicture";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";

type Tr = ReturnType<typeof useTr>;

function failureText(tr: Tr, why: string): string {
  if (why === "denied") {
    return tr(
      "rp_capdev_list_denied",
      undefined,
      "The app may not use video inputs. Allow camera access for PS5Upload in the system settings, then press Find devices.",
    );
  }
  return tr("rp_capdev_list_failed", undefined, "The video inputs could not be listed. Press Find devices to try again.");
}

export function CaptureSetting({ running }: { running: boolean }) {
  const tr = useTr();
  const host = useRosterStore((s) => s.profiles.find((p) => p.id === s.active_id)?.host ?? null);
  const consoleName = useRosterStore((s) => s.profiles.find((p) => p.id === s.active_id)?.name ?? "");
  // Read again whenever a choice is saved.
  useCaptureStore((s) => s.rev);
  const setChoice = useCaptureStore((s) => s.setChoice);
  const showing = useCaptureStore((s) => !!s.media && s.host === host);
  const [devices, setDevices] = useState<DeviceInfo[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [looking, setLooking] = useState(false);
  const choice = captureChoiceFor(host);
  const on = choice.picture === "device";

  const look = async () => {
    setLooking(true);
    const r = await listCaptureDevices();
    setLooking(false);
    if (typeof r === "string") {
      setProblem(r);
      return;
    }
    setProblem(null);
    setDevices(r);
    return r;
  };

  // The list once the option is on (asking for access the first time, which needs it on).
  useEffect(() => {
    if (on && devices === null && !looking && captureAvailable()) void look();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [on]);

  // On with no device chosen yet: the first one listed.
  useEffect(() => {
    if (!host || !on || choice.deviceId || choice.label) return;
    const first = devices?.find((d) => d.kind === "videoinput");
    if (first) setChoice(host, { ...choice, deviceId: first.deviceId, label: first.label });
  }, [devices, on, host, choice, setChoice]);

  if (!captureAvailable() || !host) return null;

  const save = (c: Partial<CaptureChoice>) => setChoice(host, { ...choice, ...c });
  const video = (devices ?? []).filter((d) => d.kind === "videoinput");
  const picked = devices ? findDevice(devices, choice) : null;
  const nameOf = (d: DeviceInfo, i: number) =>
    d.label || tr("rp_capdev_unnamed", { n: String(i + 1) }, "Video input {n}");
  const options = video.map((d, i) => ({ value: d.deviceId, label: nameOf(d, i) }));
  // The saved device while it is not plugged in (or not listed yet): shown, not lost.
  if (choice.deviceId && !picked) {
    options.unshift({
      value: choice.deviceId,
      label: `${choice.label || tr("rp_capdev_saved", undefined, "Saved device")} (${tr("rp_capdev_not_found", undefined, "not found")})`,
    });
  }

  return (
    <div className="space-y-3" data-testid="rp-capture-setting">
      <Select
        label={tr("rp_capdev_picture_from", { console: consoleName }, "Picture from ({console})")}
        value={choice.picture}
        onChange={(e) => {
          const picture = e.target.value === "device" ? "device" : "rp";
          // The first device listed, when none was chosen yet.
          const first = picture === "device" && !choice.deviceId ? video[0] : undefined;
          save({ picture, ...(first ? { deviceId: first.deviceId, label: first.label } : {}) });
        }}
        data-testid="rp-capture-source"
        options={[
          { value: "rp", label: tr("rp_capdev_from_rp", undefined, "Remote Play") },
          { value: "device", label: tr("rp_capdev_from_device", undefined, "A capture device") },
        ]}
      />
      {on && (
        <>
          <div className="flex items-end gap-2">
            <div className="min-w-0 flex-1">
              <Select
                label={tr("rp_capdev_device", undefined, "Capture device")}
                value={picked?.deviceId ?? choice.deviceId}
                onChange={(e) => {
                  const d = video.find((v) => v.deviceId === e.target.value);
                  if (d) save({ deviceId: d.deviceId, label: d.label });
                }}
                data-testid="rp-capture-device"
                options={
                  options.length
                    ? options
                    : [{ value: "", label: tr("rp_capdev_none", undefined, "No video input found") }]
                }
              />
            </div>
            <Button size="sm" onClick={() => void look()} disabled={looking} data-testid="rp-capture-find">
              {tr("rp_capdev_find", undefined, "Find devices")}
            </Button>
          </div>
          {problem && <p className="text-xs text-[var(--color-danger,#dc2626)]">{failureText(tr, problem)}</p>}
          <Select
            label={tr("rp_capdev_sound_from", undefined, "Sound from")}
            value={choice.sound}
            onChange={(e) => save({ sound: e.target.value === "device" ? "device" : "rp" })}
            data-testid="rp-capture-sound"
            options={[
              { value: "rp", label: tr("rp_capdev_sound_rp", undefined, "Remote Play") },
              { value: "device", label: tr("rp_capdev_sound_device", undefined, "The capture device") },
            ]}
          />
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "rp_capdev_hint",
              undefined,
              "The console's HDMI goes into the capture device, and the capture device into this computer. Remote Play still carries the controller, keyboard and mouse, at 720p, 30 fps and 3 Mbps to keep its load low; that part applies when you start again. If the device stops, the Remote Play picture comes back.",
            )}
            {running && showing ? ` ${tr("rp_capdev_showing", undefined, "Showing the capture device now.")}` : ""}
          </p>
        </>
      )}
    </div>
  );
}
