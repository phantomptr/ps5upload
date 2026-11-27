import { useState } from "react";
import { ListChecks } from "lucide-react";

import { Button } from "../../components";
import { ArchiveChooser } from "../../components/ArchiveChooser";
import type { ArchiveChoice } from "../../lib/archiveChoice";
import { formatBytes } from "../../lib/format";
import { useTr } from "../../state/lang";

/** What is chosen of an archive, in a few words: everything, or how many items. */
export function ArchiveChoiceText({ count, bytes }: { count: number | null; bytes?: number | null }) {
  const tr = useTr();
  if (count === null) return <>{tr("archive_choose_whole", undefined, "Everything in the archive is sent.")}</>;
  return (
    <>
      {bytes
        ? tr("archive_choose_some_size", { n: count, size: formatBytes(bytes) }, "Only {n} chosen item(s) are sent ({size}).")
        : tr("archive_choose_some", { n: count }, "Only {n} chosen item(s) are sent.")}
    </>
  );
}

/**
 * "Choose files…" for an archive that is unpacked onto the PS5 (#969): the line that says what
 * goes, and the chooser behind its button. `compact` is the queue row's one line.
 */
export function ArchiveChooseCard({
  archivePath,
  password = null,
  chosen,
  chosenBytes = null,
  onChoose,
  compact = false,
  disabled = false,
}: {
  archivePath: string;
  password?: string | null;
  /** The chosen paths, or null for everything. */
  chosen: readonly string[] | null;
  chosenBytes?: number | null;
  onChoose: (choice: ArchiveChoice) => void;
  compact?: boolean;
  disabled?: boolean;
}) {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  const button = (
    <Button
      size="sm"
      variant="secondary"
      leftIcon={<ListChecks size={14} />}
      onClick={() => setOpen(true)}
      disabled={disabled}
      data-testid="archive-choose-open"
    >
      {tr("archive_choose_open", undefined, "Choose files…")}
    </Button>
  );
  const chooser = open && (
    <ArchiveChooser
      open
      archivePath={archivePath}
      password={password}
      chosen={chosen}
      onCancel={() => setOpen(false)}
      onConfirm={(c) => {
        setOpen(false);
        onChoose(c);
      }}
    />
  );
  const text = <ArchiveChoiceText count={chosen ? chosen.length : null} bytes={chosenBytes} />;
  if (compact) {
    return (
      <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-[var(--color-muted)]" data-testid="archive-choose-line">
        <span className="min-w-0">{text}</span>
        {button}
        {chooser}
      </div>
    );
  }
  return (
    <section
      className="mb-3 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-4 shadow-[var(--edge-highlight),var(--shadow-1)]"
      data-testid="archive-choose-card"
    >
      <h3 className="mb-1 text-sm font-semibold">{tr("archive_choose_heading", undefined, "What to send")}</h3>
      <div className="flex flex-wrap items-center gap-2 text-xs text-[var(--color-muted)]">
        <span className="min-w-0 flex-1">{text}</span>
        {button}
      </div>
      {chooser}
    </section>
  );
}
