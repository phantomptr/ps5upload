// The Workshop check list's size line for a PS5 package: whether the console the package is
// for can open one this big (each firmware line has its own ceiling, see lib/packageLimit), with
// the size, the ceiling and what is left; past it, an error and the way out (a game image).

import type { FpkgCompression, FpkgEstimates, FpkgInspection } from "../../api/fpkg";
import { Button } from "../../components";
import {
  bandLabel,
  gib,
  LARGEST_MEASURED_INDEX,
  packageFit,
  withContainer,
  type PackageFit,
} from "../../lib/packageLimit";
import { useTr } from "../../state/lang";

/** The package's fit for this game at this compression level: the level's sampled size, or the
 *  uncompressed size for Stored. Null while a compressed size is still being worked out and
 *  the uncompressed one would not fit (no verdict yet), and for anything not a PS5 package. */
export function fitFor(
  inspection: FpkgInspection,
  compression: FpkgCompression,
  estimates: FpkgEstimates | "pending" | null | undefined,
  consoleFirmware: string | null | undefined,
  target?: string | null,
): PackageFit | null {
  if (inspection.ps4_game || inspection.ps4_kind || !inspection.planned_size) return null;
  const common = {
    target,
    console: consoleFirmware,
    required: inspection.required_firmware,
    sizes: inspection.param_sizes,
    layoutIndexFixed: inspection.layout_index_fixed,
  };
  const stored = packageFit({ ...common, size: withContainer(inspection.planned_size), estimated: false });
  if (compression === "stored") return stored;
  const level = estimates && estimates !== "pending" ? estimates[compression] : null;
  if (level) return packageFit({ ...common, size: withContainer(level.bytes), estimated: true });
  // Compressed, size not known yet: the uncompressed size is an upper bound, so it can only
  // say "fits".
  return stored.fits ? stored : null;
}

export function PackageFitLine({ fit, onMakeImage }: { fit: PackageFit; onMakeImage?: () => void }) {
  const tr = useTr();
  const fw = fit.limit ? bandLabel(fit.limit.band) : `${fit.firmware.major}.${String(fit.firmware.minor).padStart(2, "0")}`;
  const approx = fit.estimated ? "~" : "";
  const index =
    fit.layoutIndex != null && fit.layoutIndex > LARGEST_MEASURED_INDEX ? (
      <div className="text-[var(--color-warn)]" data-testid="layout-index-large">
        {tr(
          "fpkg.layoutIndexLarge",
          { mib: (fit.layoutIndex / 2 ** 20).toFixed(1), max: (LARGEST_MEASURED_INDEX / 2 ** 20).toFixed(1) },
          "Its layout index would be about {mib} MiB, more than any package measured so far ({max} MiB). The PS5 loads it whole before the game starts; if the game doesn't start, make a game image instead.",
        )}
      </div>
    ) : null;
  if (!fit.limit) {
    return (
      <div className="text-[var(--color-muted)]" data-testid="package-fit">
        {tr("fpkg.sizeNoLimit", { fw }, "No package size limit is known on FW {fw}.")}
        {index}
      </div>
    );
  }
  const { cap, reserved } = fit.limit;
  if (!fit.fits) {
    const vars = { size: `${approx}${gib(fit.size)}`, fw, cap: gib(cap), addon: gib(reserved) };
    return (
      <div className="flex flex-col gap-1 text-[var(--color-bad)]" data-testid="package-fit" role="alert">
        <div>
          ✕{" "}
          {reserved > 0
            ? tr(
                "fpkg.sizeTooBigAddon",
                vars,
                "Too big for the PS5: this package would be {size} GiB, and FW {fw} can't open a fake package larger than {cap} GiB including the add-on space this game reserves ({addon} GiB). It would install but never start. Make a game image instead, or use the original package.",
              )
            : tr(
                "fpkg.sizeTooBig",
                vars,
                "Too big for the PS5: this package would be {size} GiB, and FW {fw} can't open a fake package larger than {cap} GiB. It would install but never start. Make a game image instead, or use the original package.",
              )}
        </div>
        {onMakeImage && (
          <div>
            <Button size="sm" variant="secondary" onClick={onMakeImage} data-testid="make-image-instead">
              {tr("fpkg.makeImageInstead", undefined, "Make a game image instead")}
            </Button>
          </div>
        )}
      </div>
    );
  }
  const used = fit.size + reserved;
  const vars = {
    used: `${approx}${gib(used)}`,
    cap: gib(cap),
    fw,
    addon: gib(reserved),
    spare: gib(Math.max(0, cap - used)),
  };
  return (
    <div className="flex flex-col gap-0.5" data-testid="package-fit">
      <div className="text-[var(--color-good)]">
        ✓{" "}
        {reserved > 0
          ? tr(
              "fpkg.sizeFitsAddon",
              vars,
              "Fits the PS5's {cap} GiB limit on FW {fw}: {used} of {cap} GiB incl. {addon} GiB add-on space, {spare} GiB to spare",
            )
          : tr("fpkg.sizeFits", vars, "Fits the PS5's {cap} GiB limit on FW {fw}: {used} of {cap} GiB, {spare} GiB to spare")}
      </div>
      {fit.olderMiss && (
        <div className="text-[var(--color-muted)]" data-testid="package-fit-older">
          {tr(
            "fpkg.sizeOlderFirmware",
            { size: vars.used },
            "At {size} GiB it opens on FW 13.x only: FW 7.xx to 12.xx open fake packages up to 256 GiB (some games 320 GiB on 11.xx and 12.xx).",
          )}
        </div>
      )}
      {index}
    </div>
  );
}
