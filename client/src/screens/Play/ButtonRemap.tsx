import { Button, Select } from "../../components";
import { isDefaultMap, PAD_CONTROLS, setMapping, type PadControl } from "../../lib/rpButtonMap";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";

/** Change which PS5 button each controller button sends, saved on this device with the other
 *  Remote Play settings. Works on every controller the same way (lib/rpButtonMap.ts). */
export function ButtonRemap() {
  const tr = useTr();
  const map = useRemotePlayStore((s) => s.prefs.buttonMap);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const name = (id: PadControl) => {
    const c = PAD_CONTROLS.find((x) => x.id === id)!;
    // The same names as the keyboard list (rp_action_*); symbols and L1-R3 need none.
    return /[a-z]/.test(c.label) ? tr(`rp_action_${c.id}`, undefined, c.label) : c.label;
  };
  const options = [
    ...PAD_CONTROLS.map((c) => ({ value: c.id, label: name(c.id) })),
    { value: "none", label: tr("rp_buttons_none", undefined, "Nothing") },
  ];
  return (
    <div className="space-y-2" data-testid="rp-button-map">
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_buttons_hint",
          undefined,
          "What each controller button sends to the PS5. The keyboard has its own list below.",
        )}
      </p>
      <div className="grid grid-cols-1 gap-x-6 gap-y-1 sm:grid-cols-2">
        {PAD_CONTROLS.map((c) => (
          <div key={c.id} className="flex items-center justify-between gap-2">
            <span>{name(c.id)}</span>
            <Select
              block={false}
              className="w-40"
              aria-label={name(c.id)}
              value={map[c.id] ?? c.id}
              onChange={(e) =>
                setPrefs({ buttonMap: setMapping(map, c.id, e.target.value as PadControl | "none") })
              }
              data-testid={`rp-button-${c.id}`}
              options={options}
            />
          </div>
        ))}
      </div>
      <Button size="sm" disabled={isDefaultMap(map)} onClick={() => setPrefs({ buttonMap: {} })}>
        {tr("rp_keys_reset", undefined, "Reset to defaults")}
      </Button>
    </div>
  );
}
