import { useState } from "react";
import { Link2 } from "lucide-react";

import { linkProbe, type LinkClass } from "../../api/links";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { LinkDownloadCard } from "../InstallPackage/LinkDownloadCard";

/** Files › More › Download from a link (#872): paste a link and its file is saved into the
 *  folder that is open. A package is saved like any other file; nothing is installed. */
export function LinkToFolder({
  host,
  dir,
  onSaved,
  onClose,
}: {
  host: string;
  /** The folder that was open when the action was chosen. */
  dir: string;
  /** The file landed: the listing is stale. */
  onSaved: () => void;
  onClose: () => void;
}) {
  const tr = useTr();
  const [url, setUrl] = useState("");
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [found, setFound] = useState<{ url: string; info: LinkClass } | null>(null);

  async function check() {
    const link = url.trim();
    if (!link || checking) return;
    setError(null);
    setChecking(true);
    try {
      const info = await linkProbe(link);
      if (info.kind === "refused") {
        setError(
          info.message ?? tr("linkdl.refused", undefined, "That link is not a file download."),
        );
        return;
      }
      setFound({ url: link, info });
    } catch (e) {
      setError(`${e}`);
    } finally {
      setChecking(false);
    }
  }

  if (found) {
    return (
      <LinkDownloadCard
        // A new link is a new card: its name and folder start from that link.
        key={found.url}
        host={host}
        url={found.url}
        info={found.info}
        insecureTls={false}
        fixedDir={dir}
        onDone={onSaved}
        onClose={onClose}
      />
    );
  }

  return (
    <div
      className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] mb-3 p-2 text-sm"
      data-testid="fs-link-download"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Link2 size={14} className="shrink-0 text-[var(--color-muted)]" />
        <span className="min-w-0 break-all text-xs text-[var(--color-muted)]">
          {tr("fs_link_into", undefined, "Download from a link into")}{" "}
          <span className="font-mono">{dir}</span>
        </span>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <input
          autoFocus
          value={url}
          placeholder="https://"
          inputMode="url"
          spellCheck={false}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void check();
            if (e.key === "Escape") onClose();
          }}
          className="input input-sm min-w-40 flex-1 text-xs"
          aria-label={tr("fs_link_field", undefined, "Link")}
          data-testid="fs-link-url"
        />
        <Button
          size="sm"
          variant="primary"
          loading={checking}
          disabled={!url.trim() || checking}
          onClick={() => void check()}
          data-testid="fs-link-check"
        >
          {checking
            ? tr("linkdl.checking", undefined, "Checking link…")
            : tr("fs_link_check", undefined, "Check link")}
        </Button>
        <Button size="sm" variant="ghost" onClick={onClose}>
          {tr("cancel", undefined, "Cancel")}
        </Button>
      </div>
      {error && (
        <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}
    </div>
  );
}
