// A .pkg on the Upload screen: install it (the default) or copy it to the destination folder as
// a plain file, for a mounter that reads packages where they sit (Kraken Mount). Short labels so
// the two buttons fit a phone's width.

import type { PkgChoice } from "../../lib/pkgUploadChoice";
import { useTr } from "../../state/lang";

export function PkgChoiceField({
  choice,
  onChoose,
  disabled = false,
}: {
  choice: PkgChoice;
  onChoose: (c: PkgChoice) => void;
  disabled?: boolean;
}) {
  const tr = useTr();
  const opts: [PkgChoice, string][] = [
    ["install", tr("upload_pkg_choice_install", undefined, "Install")],
    ["copy", tr("upload_pkg_choice_copy", undefined, "Copy as a file")],
  ];
  return (
    <div
      role="radiogroup"
      aria-label={tr("upload_pkg_choice_label", undefined, "What to do with the package")}
      className="inline-flex max-w-full overflow-hidden rounded-[var(--radius-field)] border border-[var(--color-border)] text-xs"
    >
      {opts.map(([value, label]) => (
        <button
          key={value}
          type="button"
          role="radio"
          aria-checked={choice === value}
          disabled={disabled}
          data-testid={`pkg-choice-${value}`}
          onClick={() => {
            if (choice !== value) onChoose(value);
          }}
          className={
            "whitespace-nowrap px-2 py-0.5 disabled:opacity-60 " +
            (choice === value
              ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
              : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]")
          }
        >
          {label}
        </button>
      ))}
    </div>
  );
}

/** The one line under a copied package: where Kraken Mount looks for it. */
export function PkgCopyHint({ krakenRuns }: { krakenRuns: boolean }) {
  const tr = useTr();
  return (
    <p className="text-xs text-[var(--color-muted)]" data-testid="pkg-copy-hint">
      {krakenRuns
        ? tr(
            "upload_pkg_copy_kraken",
            undefined,
            "Copied, not installed. Kraken Mount runs on this PS5, so the folder is set to one it scans.",
          )
        : tr(
            "upload_pkg_copy_hint",
            undefined,
            "Copied, not installed. Kraken Mount only finds packages in the folders it scans, such as /data/homebrew.",
          )}
    </p>
  );
}
