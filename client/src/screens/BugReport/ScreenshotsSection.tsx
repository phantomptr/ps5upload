import { useEffect, useRef, useState } from "react";
import { Camera, Check, ImagePlus, X } from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { invoke } from "../../lib/invokeLogged";
import { isTauriEnv } from "../../lib/tauriEnv";
import { fileToBase64 } from "../../lib/browserBugBundle";
import { SCREENSHOT_EVENT, type SavedShot } from "../../lib/captureScreenshot";

/** Images that go in the zip: the app's own captures (desktop, by path) and added files. */
export interface Attached {
  shots: SavedShot[];
  files: { name: string; base64: string; preview: string }[];
}

const isImage = (f: File) => f.type.startsWith("image/");
const thumb = "aspect-video w-full rounded-md object-cover";

/**
 * Pick from the captures the desktop app's Capture button (status bar, bottom right) has taken,
 * and add any other image by picking, dropping or pasting it. A capture taken while the report is
 * open is added to it.
 */
export default function ScreenshotsSection({
  attached,
  setAttached,
}: {
  attached: Attached;
  setAttached: (fn: (a: Attached) => Attached) => void;
}) {
  const tr = useTr();
  const desktop = isTauriEnv();
  const input = useRef<HTMLInputElement>(null);
  const [captures, setCaptures] = useState<SavedShot[]>([]);
  const [over, setOver] = useState(false);

  useEffect(() => {
    if (!desktop) return;
    void invoke<SavedShot[]>("screenshot_list", { limit: 24 })
      .then((list) => setCaptures(list ?? []))
      .catch(() => setCaptures([]));
    const onCapture = (e: Event) => {
      const shot = (e as CustomEvent<SavedShot>).detail;
      setCaptures((c) => [shot, ...c.filter((x) => x.name !== shot.name)]);
      setAttached((a) => (a.shots.some((x) => x.name === shot.name) ? a : { ...a, shots: [...a.shots, shot] }));
    };
    window.addEventListener(SCREENSHOT_EVENT, onCapture);
    return () => window.removeEventListener(SCREENSHOT_EVENT, onCapture);
  }, [desktop, setAttached]);

  const add = async (list: Iterable<File>) => {
    const files = await Promise.all(
      [...list].filter(isImage).map(async (file, i) => {
        const base64 = await fileToBase64(file);
        // A pasted image is always "image.png": number them so they don't overwrite each other.
        const name = file.name && file.name !== "image.png" ? file.name : `pasted-${Date.now()}-${i}.png`;
        return { name, base64, preview: `data:${file.type};base64,${base64}` };
      }),
    );
    if (files.length) setAttached((a) => ({ ...a, files: [...a.files, ...files] }));
  };
  // A pasted image anywhere on the page is added (text pastes carry no files).
  const addRef = useRef(add);
  useEffect(() => {
    addRef.current = add;
  });
  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      if (e.clipboardData?.files.length) void addRef.current(e.clipboardData.files);
    };
    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, []);

  const picked = (s: SavedShot) => attached.shots.some((x) => x.name === s.name);
  const toggle = (s: SavedShot) =>
    setAttached((a) => ({
      ...a,
      shots: a.shots.some((x) => x.name === s.name) ? a.shots.filter((x) => x.name !== s.name) : [...a.shots, s],
    }));

  return (
    <div className="grid gap-4">
      {desktop && (
        <div className="grid gap-3">
          <p className="flex items-start gap-2 text-xs leading-relaxed text-[var(--color-muted)]" data-testid="br-capture-hint">
            <span className="mt-px inline-flex shrink-0 items-center gap-1 rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-[var(--color-text)]">
              <Camera size={12} />
              {tr("status_capture", undefined, "Capture")}
            </span>
            <span>
              {tr(
                "br_capture_hint",
                undefined,
                "Use the Capture button at the bottom right of the window, in the status bar, to take a picture of any screen. Go to the screen with the problem, capture it, then come back: it is added here.",
              )}
            </span>
          </p>
          {captures.length > 0 && (
            <div>
              <p className="mb-2 text-xs font-medium">
                {tr("br_captures_title", undefined, "Your captures: pick the ones to include")}
              </p>
              <ul className="grid grid-cols-[repeat(auto-fill,minmax(8rem,1fr))] gap-3" data-testid="br-captures">
                {captures.map((s) => {
                  const on = picked(s);
                  return (
                    <li key={s.name}>
                      <button
                        type="button"
                        aria-pressed={on}
                        aria-label={s.name}
                        onClick={() => toggle(s)}
                        className={
                          "relative block w-full rounded-lg border-2 p-0.5 transition-colors " +
                          (on
                            ? "border-[var(--color-accent)]"
                            : "border-transparent opacity-75 hover:border-[var(--color-border-strong)] hover:opacity-100")
                        }
                      >
                        <img src={s.data_url} alt="" className={thumb} />
                        <span
                          className={
                            "absolute right-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded-full border " +
                            (on
                              ? "border-[var(--color-accent)] bg-[var(--color-accent)] text-white"
                              : "border-white/70 bg-black/40")
                          }
                        >
                          {on && <Check size={12} strokeWidth={3} />}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      )}

      {attached.files.length > 0 && (
        <ul className="grid grid-cols-[repeat(auto-fill,minmax(8rem,1fr))] gap-3" data-testid="br-attached">
          {attached.files.map((f) => (
            <li key={f.name} className="relative">
              <img src={f.preview} alt={f.name} className={`${thumb} border border-[var(--color-border)]`} />
              <button
                type="button"
                aria-label={`${tr("br_remove", undefined, "Remove")} ${f.name}`}
                onClick={() => setAttached((a) => ({ ...a, files: a.files.filter((x) => x !== f) }))}
                className="absolute right-1.5 top-1.5 rounded-full bg-black/70 p-1 text-white"
              >
                <X size={12} />
              </button>
            </li>
          ))}
        </ul>
      )}

      <div
        data-testid="br-dropzone"
        onDragOver={(e) => (e.preventDefault(), setOver(true))}
        onDragLeave={() => setOver(false)}
        onDrop={(e) => (e.preventDefault(), setOver(false), void add(e.dataTransfer.files))}
        className={
          "flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-dashed px-4 py-3 " +
          (over ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]" : "border-[var(--color-border)]")
        }
      >
        <Button variant="secondary" size="sm" leftIcon={<ImagePlus size={14} />} onClick={() => input.current?.click()}>
          {tr("br_add_images", undefined, "Add images")}
        </Button>
        <span className="text-xs text-[var(--color-muted)]">
          {tr("br_shots_hint", undefined, "Or drop them here, or paste one with Ctrl/⌘+V.")}
        </span>
        <input
          ref={input}
          type="file"
          accept="image/*"
          multiple
          hidden
          aria-label={tr("br_attach", undefined, "Attach screenshots")}
          onChange={(e) => {
            if (e.target.files) void add(e.target.files);
            e.target.value = "";
          }}
        />
      </div>
    </div>
  );
}
