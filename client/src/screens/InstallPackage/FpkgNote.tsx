import { CheckCircle2, ChevronDown, Info } from "lucide-react";

import { PPR_PATCH_REPO } from "../../lib/fpkgPrereqs";
import { krakenRuns, useMounterFor } from "../../lib/mounter";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";

/**
 * The two ways to play a fake PS5 package (FPKG), side by side and with equal weight: install it
 * (kstuff; on 11.61 to 13.60 only the latest kstuff, with fake-package support up to 13.60,
 * starts the game) or mount it with Kraken Mount (a debug-key PS5 package copied as a plain file
 * to a folder it scans). Stated rather than detected: no header marker separates a fake package
 * from a retail one, so the reader decides whether it applies.
 *
 * The only running-payload status shown is Kraken Mount's, from the mounter snapshot this screen
 * already reads (KrakenMountCard and Upload fill it through useSmpState). Whether kstuff runs is
 * not read here: nothing on this screen asks the console for its process list.
 */
export function FpkgNote({ className = "mb-4" }: { className?: string }) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const kraken = krakenRuns(useMounterFor(host));
  const send = tr("payloads_tab_send", "Send payload");
  const copy = tr("upload_pkg_choice_copy", "Copy as a file");
  const column =
    "flex min-w-0 flex-col gap-1.5 rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] p-2.5";
  const heading = "font-medium text-[var(--color-text)]";
  return (
    <details
      data-testid="fpkg-note"
      className={`group ${className} rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface-2)] text-[12px] text-[var(--color-muted)]`}
    >
      <summary className="flex cursor-pointer list-none items-start gap-2 px-3 py-2 [&::-webkit-details-marker]:hidden">
        <Info size={13} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="font-medium text-[var(--color-text)]">
            {tr("pkglib.fpkgsupport.title_v2", "Two ways to play a fake PS5 package")}
          </span>
          <span>
            {tr(
              "pkglib.fpkgsupport.summary",
              "Install it with kstuff loaded, or mount it with Kraken Mount. PS4 fake packages are installed, and play on any firmware.",
            )}
          </span>
        </span>
        <ChevronDown
          size={14}
          className="mt-0.5 shrink-0 text-[var(--color-muted)] transition-transform group-open:rotate-180"
        />
      </summary>
      <div className="flex flex-col gap-2 px-3 pb-3">
        <div className="grid gap-2 sm:grid-cols-2">
          <section data-testid="fpkg-way-install" className={column}>
            <div className={heading}>{tr("pkglib.fpkgsupport.install_title", "Install")}</div>
            <div>
              {tr(
                "pkglib.fpkgsupport.install_what",
                "The PS5 installs the package like a game from the store.",
              )}
            </div>
            <div>
              {tr(
                "pkglib.fpkgsupport.install_needs",
                "Needs kstuff running, and ShadowMount+, the mount layer that registers the installed title.",
              )}
            </div>
            <div>
              {tr(
                "pkglib.fpkgsupport.install_kstuff_v2",
                "On firmware 11.61 to 13.60 that has to be the latest kstuff (with fake-package support up to 13.60): with an older kstuff the package installs, but the game doesn't start. Up to 11.60, any kstuff with fake-package support works. If a game you installed with an older kstuff doesn't start, install it again.",
              )}
            </div>
            <div>
              {tr(
                "pkglib.fpkgsupport.install_catalog",
                { send },
                `The catalog's kstuff may be older than that; send the latest one with ${send}.`,
              )}
            </div>
            <div className="break-words">
              {tr(
                "pkglib.fpkgsupport.install_ppr_v2",
                "Up to firmware 11.40, if the console refuses the install, also load a53_ppr_install_fast.elf, built from the source on GitHub:",
              )}{" "}
              <button
                type="button"
                className="underline"
                onClick={() => void openExternalUrl(PPR_PATCH_REPO)}
              >
                {"drakmor/ppr-patch"}
              </button>
            </div>
          </section>
          <section data-testid="fpkg-way-mount" className={column}>
            <div className={heading}>
              {tr("pkglib.fpkgsupport.mount_title", "Mount with Kraken Mount")}
            </div>
            <div>
              {tr(
                "pkglib.fpkgsupport.mount_what",
                "Kraken Mount puts the game on the home screen straight from the .pkg file, without installing it.",
              )}
            </div>
            <div>
              {tr(
                "pkglib.fpkgsupport.mount_needs",
                "Needs Kraken Mount running, without ShadowMount+ beside it. It mounts PS5 packages made with the debug key only, not PS4 or retail packages.",
              )}
            </div>
            <div>
              {tr(
                "pkglib.fpkgsupport.mount_how",
                { copy },
                `Choose ${copy} for the package here and send it to a folder Kraken Mount scans, such as /data/homebrew. When Kraken Mount runs, that folder is filled in for you.`,
              )}
            </div>
            {kraken && (
              <div
                data-testid="fpkg-way-mount-running"
                className="flex items-center gap-1.5 font-medium text-[var(--color-text)]"
              >
                <CheckCircle2 size={13} className="shrink-0 text-[var(--color-good)]" />
                {tr("install.kraken.title", "Kraken Mount is running on this PS5")}
              </div>
            )}
          </section>
        </div>
        <div>
          {tr(
            "pkglib.fpkgsupport.tail_v2",
            "Retail packages need none of this: if that is what you are installing, ignore this note.",
          )}
        </div>
      </div>
    </details>
  );
}
