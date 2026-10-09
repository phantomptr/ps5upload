import { useEffect, useState } from "react";

import { Button, Input, Select } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { processList } from "../../api/ps5";
import type { ReportForm } from "../../lib/reportOutputs";
import { PLATFORMS } from "./draft";

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

/** The app, the computer and the PS5: filled in where the app knows, editable. */
export default function SetupSection({
  form: f,
  updateForm,
}: {
  form: ReportForm;
  updateForm: (p: Partial<ReportForm>) => void;
}) {
  const tr = useTr();
  const kernel = useConnectionStore((s) => s.ps5Kernel);
  const [reading, setReading] = useState(false);

  useEffect(() => {
    if (!f.firmware && kernel) updateForm({ firmware: firmwareFromKernel(kernel) });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kernel]);

  const readPayloads = async () => {
    if (!f.console) return;
    setReading(true);
    try {
      const r = await processList(mgmtAddr(f.console));
      updateForm({ payloads: payloadsFromProcesses(r.processes.map((p) => p.name)) });
    } catch {
      // console not answering: the field stays for typing
    } finally {
      setReading(false);
    }
  };

  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <Input label={tr("br_app_version", undefined, "App version")} value={f.appVersion} onChange={(e) => updateForm({ appVersion: e.target.value })} />
      <Select
        label={tr("br_platform", undefined, "Platform")}
        value={f.platform}
        onChange={(e) => updateForm({ platform: e.target.value })}
        options={PLATFORMS.map((p) => ({ value: p, label: p }))}
      />
      <Input label={tr("br_firmware", undefined, "PS5 firmware")} value={f.firmware} placeholder="13.60" onChange={(e) => updateForm({ firmware: e.target.value })} />
      <Input
        label={tr("br_model", undefined, "PS5 model (optional)")}
        value={f.model}
        placeholder={tr("br_model_placeholder", undefined, "CFI-7019")}
        onChange={(e) => updateForm({ model: e.target.value })}
      />
      <div className="flex flex-col gap-2 sm:col-span-2 sm:flex-row sm:items-end">
        <div className="min-w-0 flex-1">
          <Input
            label={tr("br_payloads", undefined, "Other payloads loaded")}
            value={f.payloads}
            placeholder={tr("br_payloads_placeholder", undefined, "kstuff-lite 1.11, ShadowMountPlus 1.7beta4")}
            onChange={(e) => updateForm({ payloads: e.target.value })}
          />
        </div>
        <Button variant="secondary" disabled={!f.console || reading} loading={reading} onClick={() => void readPayloads()}>
          {tr("br_payloads_read", undefined, "Read them from the console")}
        </Button>
      </div>
    </div>
  );
}
