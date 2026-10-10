// The viewer's Images tab: every piece of artwork the source carries, to look at and save.

import { useEffect, useState } from "react";
import { Download } from "lucide-react";

import { gameInspectImageUrl } from "../../api/gameInspect";
import { formatBytes } from "../../lib/format";
import { useTr } from "../../state/lang";
import { Spinner } from "../index";

function ImageCard({ token, name, size }: { token: string; name: string; size: number }) {
  const tr = useTr();
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let live = true;
    void gameInspectImageUrl(token, name)
      .then((u) => live && setUrl(u))
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [token, name]);
  return (
    <figure className="grid gap-1.5 rounded-[var(--radius-card)] border border-[var(--color-border)] p-3">
      <div className="flex aspect-video items-center justify-center overflow-hidden rounded bg-[var(--color-surface-2)]">
        {url ? (
          <img src={url} alt={name} loading="lazy" className="max-h-full max-w-full object-contain" />
        ) : failed ? (
          <span className="text-xs text-[var(--color-muted)]">
            {tr("viewer_image_unreadable", undefined, "Can't read this image")}
          </span>
        ) : (
          <Spinner size={14} />
        )}
      </div>
      <figcaption className="flex items-center justify-between gap-2 text-xs">
        <span className="truncate font-mono">{name}</span>
        <span className="flex shrink-0 items-center gap-2 text-[var(--color-muted)]">
          {formatBytes(size)}
          {url && (
            <a
              href={url}
              download={name}
              className="inline-flex items-center gap-1 text-[var(--color-accent)] hover:underline"
            >
              <Download size={12} />
              {tr("viewer_image_save", undefined, "Save")}
            </a>
          )}
        </span>
      </figcaption>
    </figure>
  );
}

export function ImagesTab({ token, images }: { token: string; images: { name: string; size: number }[] }) {
  const tr = useTr();
  if (images.length === 0) {
    return (
      <div className="text-sm text-[var(--color-muted)]">
        {tr("viewer_images_none", undefined, "No artwork is readable in this source.")}
      </div>
    );
  }
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      {images.map((i) => (
        <ImageCard key={i.name} token={token} name={i.name} size={i.size} />
      ))}
    </div>
  );
}
