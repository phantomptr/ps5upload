// The one switch for "use less of this computer" (#632), shown in Settings and beside the
// Workshop's running conversion and queue, where people look for it.

import { useEffect } from "react";

import { Toggle } from "../../components";
import { useTr } from "../../state/lang";
import { useWorkshopSettings } from "../../state/workshopSettings";

export function GentleToggle({ compact = false }: { compact?: boolean }) {
  const tr = useTr();
  const gentle = useWorkshopSettings((s) => s.gentle);
  const busy = useWorkshopSettings((s) => s.busy);
  const error = useWorkshopSettings((s) => s.error);
  const load = useWorkshopSettings((s) => s.load);
  const setGentle = useWorkshopSettings((s) => s.setGentle);
  useEffect(() => {
    void load();
  }, [load]);
  return (
    <div className="flex flex-col gap-1" data-testid="gentle-toggle">
      <Toggle
        checked={gentle}
        disabled={busy}
        onChange={(on) => void setGentle(on)}
        label={tr("workshop_gentle_label", undefined, "Use less of this computer")}
        hint={
          compact
            ? undefined
            : tr(
                "workshop_gentle_hint",
                undefined,
                "Converting uses about half your processor and lets other apps go first, for the processor and the disk. It takes longer.",
              )
        }
      />
      {error && (
        <div className="text-xs text-[var(--color-bad)]">
          {tr("workshop_gentle_error", { error }, "The engine did not keep this setting: {error}")}
        </div>
      )}
    </div>
  );
}
