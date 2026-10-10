/**
 * Whether a PS5 model has a disc drive, from its model code (HW_INFO's `model`,
 * e.g. "CFI-1115A").
 *
 * The letter after the four digits is the edition: A has a drive, B is the
 * Digital Edition. The original Digital Edition (CFI-1xxxB) can never take a
 * drive; the slim Digital Edition (CFI-2xxxB) and the Pro (CFI-7xxx, no letter)
 * ship without one but can have the add-on drive attached. Anything we can't
 * parse is "unknown", and the controls stay rather than vanish on a guess.
 */
export type DiscDrive = "yes" | "no" | "attachable" | "unknown";

export function discDriveOf(model: string | null | undefined): DiscDrive {
  const m = /^CFI-(\d)\d{3}([A-Z])?/i.exec((model ?? "").trim());
  if (!m) return "unknown";
  const series = m[1];
  const edition = m[2]?.toUpperCase();
  if (series === "7") return "attachable";
  if (edition === "A") return "yes";
  if (edition === "B") return series === "1" ? "no" : "attachable";
  return "unknown";
}
