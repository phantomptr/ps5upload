import { useEffect, useState } from "react";
import { Radio, Square } from "lucide-react";

import { Button, Modal, Toggle } from "../../components";
import { writeClipboard } from "../../lib/clipboard";
import { formatCollectionBytes } from "../../lib/collectionView";
import {
  loadPkgLinks,
  setPkgLinkOpen,
  stopPkgLinks,
  usePkgLinksStore,
} from "../../state/pkgLinks";
import { useTr } from "../../state/lang";

/** "Serving n": the install links this computer is hosting, with Stop. Hidden when none. */
export function ServingLinks() {
  const tr = useTr();
  const links = usePkgLinksStore((s) => s.links);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    void loadPkgLinks();
  }, []);
  // Live counters while the list is open, or while anything is served.
  useEffect(() => {
    if (!open && links.length === 0) return;
    const id = setInterval(() => void loadPkgLinks(), open ? 2000 : 10000);
    return () => clearInterval(id);
  }, [open, links.length]);

  if (links.length === 0 && !open) return null;
  return (
    <>
      <Button
        size="sm"
        variant="secondary"
        leftIcon={<Radio size={12} />}
        onClick={() => setOpen(true)}
      >
        {tr("collection.serving", { n: links.length }, "Serving {n}")}
      </Button>
      <Modal
        open={open}
        onClose={() => setOpen(false)}
        size="lg"
        title={tr("collection.serving_title", undefined, "Install links")}
        bodyClassName="p-4 sm:p-5"
      >
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "collection.serving_body_v2",
            undefined,
            "Packages this computer is serving to a PS5 that fetches them itself. A link works for the PS5 it was made for (or any device on your network, if you allow it) until you stop it or the app quits.",
          )}
        </p>
        <ul className="mt-3 flex flex-col gap-2">
          {links.map((l) => (
            <li
              key={l.id}
              className="rounded-2xl border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3 text-xs"
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium text-[var(--color-text)]">
                  {l.title}
                </span>
                <span className="ml-auto tabular-nums text-[var(--color-muted)]">
                  {l.requests_served > 0
                    ? tr(
                        "collection.serving_sent",
                        {
                          sent: formatCollectionBytes(l.transfer_bytes),
                          total: formatCollectionBytes(l.total_size),
                        },
                        "{sent} of {total} sent",
                      )
                    : tr(
                        "collection.serving_waiting",
                        undefined,
                        "Not fetched yet",
                      )}
                </span>
              </div>
              <div className="mt-1 break-all font-mono text-[0.6875rem] text-[var(--color-muted)]">
                {l.url}
              </div>
              <Toggle
                className="mt-2"
                checked={l.any_device}
                onChange={(on) => void setPkgLinkOpen(l.id, on)}
                label={tr(
                  "collection.serving_any",
                  undefined,
                  "Any device on this network",
                )}
                hint={tr(
                  "collection.serving_any_hint",
                  undefined,
                  "For a package installer on another console. The link's random code is what keeps the rest of your library private.",
                )}
              />
              <div className="mt-2 flex flex-wrap gap-1.5">
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => void writeClipboard(l.url)}
                >
                  {tr("collection.copy_url", undefined, "Copy link")}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<Square size={11} />}
                  onClick={() => void stopPkgLinks([l.id])}
                >
                  {tr("collection.stop", undefined, "Stop")}
                </Button>
              </div>
            </li>
          ))}
        </ul>
        {links.length > 1 && (
          <div className="mt-3 flex flex-wrap justify-end gap-2">
            <Button
              size="sm"
              variant="secondary"
              onClick={() =>
                void writeClipboard(links.map((l) => l.url).join("\n"))
              }
            >
              {tr("collection.copy_all_links", undefined, "Copy all links")}
            </Button>
            <Button
              size="sm"
              variant="danger"
              onClick={() => void stopPkgLinks(links.map((l) => l.id))}
            >
              {tr("collection.stop_all", undefined, "Stop all")}
            </Button>
          </div>
        )}
      </Modal>
    </>
  );
}
