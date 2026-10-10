/**
 * Named fan targets for the Console screen's fan card.
 *
 * The value is the temperature the console's fan control works to hold, so a
 * LOWER value is the louder, cooler one and a higher value the quieter one.
 * None of them is the console's own setting: that is far higher (91 °C on
 * FW 13.60), and "Use the console's own setting" is how to get it back.
 * Values must be inside [FAN_THRESHOLD_MIN_C, FAN_THRESHOLD_MAX_C].
 */
export interface FanPreset {
  id: "cool" | "balanced" | "quiet";
  labelKey: string;
  labelFallback: string;
  c: number;
  hintKey: string;
  hintFallback: string;
}

export const FAN_PRESETS: readonly FanPreset[] = [
  {
    id: "cool",
    labelKey: "hw_fan_preset_cool",
    labelFallback: "Cool",
    c: 55,
    hintKey: "hw_fan_preset_cool_hint",
    hintFallback: "Holds the console cooler; the fans run louder",
  },
  {
    id: "balanced",
    labelKey: "hw_fan_preset_balanced",
    labelFallback: "Balanced",
    c: 65,
    hintKey: "hw_fan_preset_balanced_hint_v2",
    hintFallback: "Between Cool and Quiet",
  },
  {
    id: "quiet",
    labelKey: "hw_fan_preset_quiet",
    labelFallback: "Quiet",
    c: 75,
    hintKey: "hw_fan_preset_quiet_hint_v2",
    hintFallback: "Quieter fans; the console runs warmer",
  },
];
