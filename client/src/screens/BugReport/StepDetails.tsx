import { useEffect, useState } from "react";

import { Button, Input, Select, Textarea, Checkbox } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { invoke } from "../../lib/invokeLogged";
import { mgmtAddr } from "../../lib/addr";
import { processList } from "../../api/ps5";
import { isTauriEnv } from "../../lib/tauriEnv";
import { fileToBase64 } from "../../lib/browserBugBundle";
import type { SavedShot } from "../../lib/captureScreenshot";
import { PLATFORMS, type WizardDraft } from "./draft";

/** Screenshots attached to the report: gallery shots (desktop, by path) and picked files (browser). */
export interface Attached {
  shots: SavedShot[];
  files: { name: string; base64: string }[];
}

/** "r229358/releases/13.60 Jul 17 2026" -> "13.60". */
export function firmwareFromKernel(kernel: string | null): string {
  return kernel?.match(/releases\/(\d+\.\d+)/)?.[1] ?? "";
}

/** Homebrew payloads in a process list: `*.elf` names other than ours and the system's. */
export function payloadsFromProcesses(names: string[]): string {
  const seen = new Set<string>();
  for (const n of names) {
    if (!/\.elf$/i.test(n) || /^ps5upload/i.test(n) || /^Sce|^mini-syscore|^fs_cleaner|^AgcCompositor|^orbis_audiod/i.test(n)) continue;
    seen.add(n.replace(/\.elf$/i, ""));
  }
  return [...seen].sort().join(", ");
}

/** Step 2: steps, how often, since when, the environment (filled in, editable) and screenshots. */
export default function StepDetails({
  draft,
  updateForm,
  attached,
  setAttached,
}: {
  draft: WizardDraft;
  updateForm: (p: Partial<WizardDraft["form"]>) => void;
  attached: Attached;
  setAttached: (a: Attached) => void;
}) {
  const tr = useTr();
  const f = draft.form;
  const kernel = useConnectionStore((s) => s.ps5Kernel);
  const [gallery, setGallery] = useState<SavedShot[]>([]);
  const [readingPayloads, setReadingPayloads] = useState(false);
  const tauri = isTauriEnv();

  useEffect(() => {
    if (!f.firmware && kernel) updateForm({ firmware: firmwareFromKernel(kernel) });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kernel]);
  useEffect(() => {
    if (!tauri) return;
    void invoke<SavedShot[]>("screenshot_list", { limit: 24 })
      .then(setGallery)
      .catch(() => setGallery([]));
  }, [tauri]);

  const readPayloads = async () => {
    if (!f.console) return;
    setReadingPayloads(true);
    try {
      const r = await processList(mgmtAddr(f.console));
      updateForm({ payloads: payloadsFromProcesses(r.processes.map((p) => p.name)) });
    } catch {
      // console not answering: the field stays for typing
    } finally {
      setReadingPayloads(false);
    }
  };
  const toggleShot = (s: SavedShot) =>
    setAttached({
      ...attached,
      shots: attached.shots.some((x) => x.name === s.name)
        ? attached.shots.filter((x) => x.name !== s.name)
        : [...attached.shots, s],
    });
  const pickFiles = async (list: FileList | null) => {
    if (!list) return;
    const files = await Promise.all([...list].map(async (file) => ({ name: file.name, base64: await fileToBase64(file) })));
    setAttached({ ...attached, files: [...attached.files, ...files] });
  };

  return (
    <div className="grid max-w-3xl gap-4">
      <Textarea
        label={tr("br_steps_label", undefined, "Steps to reproduce (optional)")}
        value={f.steps}
        rows={4}
        placeholder={"1. …\n2. …"}
        onChange={(e) => updateForm({ steps: e.target.value })}
      />
      <div className="grid gap-3 sm:grid-cols-2">
        <Select
          label={tr("br_frequency", undefined, "How often?")}
          value={f.frequency}
          onChange={(e) => updateForm({ frequency: e.target.value as WizardDraft["form"]["frequency"] })}
          options={[
            { value: "", label: "—" },
            { value: "once", label: tr("br_freq_once", undefined, "Once") },
            { value: "sometimes", label: tr("br_freq_sometimes", undefined, "Sometimes") },
            { value: "every_time", label: tr("br_freq_every", undefined, "Every time") },
          ]}
        />
        <Select
          label={tr("br_started", undefined, "When did it start?")}
          value={f.started}
          onChange={(e) => updateForm({ started: e.target.value as WizardDraft["form"]["started"] })}
          options={[
            { value: "", label: "—" },
            { value: "after_app_update", label: tr("br_started_app", undefined, "After updating the app") },
            { value: "after_fw_update", label: tr("br_started_fw", undefined, "After a firmware update") },
            { value: "always", label: tr("br_started_always", undefined, "Always") },
            { value: "unknown", label: tr("br_started_unknown", undefined, "Don't know") },
          ]}
        />
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Input label={tr("br_app_version", undefined, "App version")} value={f.appVersion} onChange={(e) => updateForm({ appVersion: e.target.value })} />
        <Select
          label={tr("br_platform", undefined, "Platform")}
          value={f.platform}
          onChange={(e) => updateForm({ platform: e.target.value })}
          options={PLATFORMS.map((p) => ({ value: p, label: p }))}
        />
        <Input label={tr("br_firmware", undefined, "PS5 firmware")} value={f.firmware} placeholder="13.60" onChange={(e) => updateForm({ firmware: e.target.value })} />
        <Input label={tr("br_model", undefined, "PS5 model (optional)")} value={f.model} placeholder={tr("br_model_placeholder", undefined, "CFI-7019")} onChange={(e) => updateForm({ model: e.target.value })} />
      </div>
      <div>
        <Textarea
          label={tr("br_payloads", undefined, "Other payloads loaded")}
          value={f.payloads}
          rows={2}
          placeholder={tr("br_payloads_placeholder", undefined, "kstuff-lite 1.11, ShadowMountPlus 1.7beta4")}
          onChange={(e) => updateForm({ payloads: e.target.value })}
        />
        <div className="mt-1.5">
          <Button size="sm" variant="secondary" disabled={!f.console || readingPayloads} loading={readingPayloads} onClick={() => void readPayloads()}>
            {tr("br_payloads_read", undefined, "Read them from the console")}
          </Button>
        </div>
      </div>
      <section>
        <h3 className="mb-1 text-sm font-medium">{tr("br_screenshots", undefined, "Screenshots")}</h3>
        {tauri ? (
          gallery.length === 0 ? (
            <p className="text-xs text-[var(--color-muted)]">
              {tr("br_no_shots", undefined, "No screenshots yet. Use the camera button in the status bar to take one.")}
            </p>
          ) : (
            <ul className="grid grid-cols-[repeat(auto-fill,minmax(8rem,1fr))] gap-2">
              {gallery.map((s) => (
                <li key={s.name}>
                  <Checkbox
                    checked={attached.shots.some((x) => x.name === s.name)}
                    onChange={() => toggleShot(s)}
                    label={<img src={s.data_url} alt={s.name} className="h-16 w-full rounded object-cover" />}
                  />
                </li>
              ))}
            </ul>
          )
        ) : (
          <input
            type="file"
            accept="image/*"
            multiple
            aria-label={tr("br_attach", undefined, "Attach screenshots")}
            onChange={(e) => void pickFiles(e.target.files)}
            className="text-sm"
          />
        )}
        {attached.files.length > 0 && (
          <p className="mt-1 text-xs text-[var(--color-muted)]">{attached.files.map((x) => x.name).join(", ")}</p>
        )}
      </section>
    </div>
  );
}
