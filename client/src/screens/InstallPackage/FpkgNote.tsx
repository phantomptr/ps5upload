import { AlertTriangle, ChevronDown } from "lucide-react";

import { PPR_PATCH_REPO } from "../../lib/fpkgPrereqs";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { useTr } from "../../state/lang";

/**
 * What a fake package (FPKG) needs, stated rather than detected: no header marker separates a
 * fake package from a retail one, so the note names the requirement and lets the reader decide
 * whether it applies. The firmware limit is on the folded line, so it is read before anyone
 * installs: PS5 fake packages install on firmware later than 11.60 but do not run there yet;
 * PS4 fake packages are fine. The load order is behind the fold.
 */
export function FpkgNote({ className = "mb-4" }: { className?: string }) {
  const tr = useTr();
  return (
    <details
      data-testid="fpkg-note"
      className={`group ${className} rounded-[var(--radius-field)] border border-[color-mix(in_srgb,var(--color-warn)_40%,transparent)] bg-[var(--color-warn)]/5 text-[12px] text-[var(--color-muted)]`}
    >
      <summary className="flex cursor-pointer list-none items-start gap-2 px-3 py-2 [&::-webkit-details-marker]:hidden">
        <AlertTriangle size={13} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="font-medium text-[var(--color-warn)]">
            {tr("pkglib.fpkgsupport.title", "Installing a fake package (FPKG)?")}
          </span>
          <span>
            {tr(
              "pkglib.fpkgsupport.firmware",
              "PS5 fake packages (PS5 FPKG) are not supported yet on firmware later than 11.60: they install, but the game does not start. PS4 fake packages are fine.",
            )}
          </span>
        </span>
        <ChevronDown
          size={14}
          className="mt-0.5 shrink-0 text-[var(--color-warn)] transition-transform group-open:rotate-180"
        />
      </summary>
      <div className="px-3 pb-3">
        <div className="flex flex-col gap-1.5">
          <div>
            {tr(
              "pkglib.fpkgsupport.lead",
              "A PS5 fake package only installs when fake-package support is already loaded on the console. Load all three, in this order, before installing:",
            )}
          </div>
          <ol className="ms-4 flex list-decimal flex-col gap-1">
            <li>
              <strong className="text-[var(--color-text)]">kstuff</strong>{" "}
              {tr("pkglib.fpkgsupport.kstuff", "— the build with PS5 fake-package support")}
            </li>
            <li className="break-words">
              <strong className="text-[var(--color-text)]">a53_ppr_install_fast.elf</strong>{" "}
              {tr("pkglib.fpkgsupport.ppr", "— applies the PPR plaintext / no-auth patch")}{" "}
              {tr(
                "pkglib.fpkgsupport.ppr_where",
                "No download is published for it: it is built from the source on GitHub,",
              )}{" "}
              <button
                type="button"
                className="underline"
                onClick={() => void openExternalUrl(PPR_PATCH_REPO)}
              >
                {"drakmor/ppr-patch"}
              </button>
              .
            </li>
            <li>
              <strong className="text-[var(--color-text)]">shadowmountplus.elf</strong>{" "}
              {tr(
                "pkglib.fpkgsupport.smp",
                "— the mount layer that registers the installed title",
              )}
            </li>
          </ol>
          <div>
            {tr(
              "pkglib.fpkgsupport.tail",
              "Without them the console refuses the install, or takes it and then fails to mount the game. Retail and debug packages need none of this — if that is what you are installing, ignore this note.",
            )}
          </div>
        </div>
      </div>
    </details>
  );
}
