// The viewer's Files tab: a package's entries, or the files of a folder or image. Loaded only
// when the tab opens; a fixed-row window keeps a 286,000-file dump as smooth as a small one.

import { useEffect, useState } from "react";
import { Lock } from "lucide-react";

import { gameInspectFiles, type InspectFile } from "../../api/gameInspect";
import { formatBytes } from "../../lib/format";
import { filterFiles, visibleWindow } from "../../lib/viewerLists";
import { useTr } from "../../state/lang";
import { Spinner } from "../index";

const ROW = 28;
const VIEWPORT = 420;

export function FilesListView(props: {
  files: InspectFile[];
  truncated: boolean;
  query: string;
  onQuery: (q: string) => void;
  scrollTop: number;
  onScroll: (top: number) => void;
}) {
  const tr = useTr();
  const shown = filterFiles(props.files, props.query);
  const { start, end } = visibleWindow(props.scrollTop, VIEWPORT, ROW, shown.length);
  const total = shown.reduce((n, f) => n + f.size, 0);
  return (
    <div className="grid gap-2">
      <input
        type="search"
        value={props.query}
        onChange={(e) => props.onQuery(e.target.value)}
        placeholder={tr("viewer_files_search", undefined, "Search files")}
        className="rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-1.5 text-sm"
      />
      <div className="text-xs text-[var(--color-muted)]">
        {tr(
          "viewer_files_count",
          { count: shown.length.toLocaleString(), size: formatBytes(total) },
          "{count} files · {size}",
        )}
        {props.truncated && ` · ${tr("viewer_files_truncated", undefined, "list shortened")}`}
      </div>
      <div
        className="overflow-y-auto rounded-md border border-[var(--color-border)]"
        style={{ height: Math.min(VIEWPORT, Math.max(ROW, shown.length * ROW)) }}
        onScroll={(e) => props.onScroll(e.currentTarget.scrollTop)}
      >
        <div style={{ height: shown.length * ROW, position: "relative" }}>
          {shown.slice(start, end).map((f, i) => (
            <div
              key={f.path}
              className="absolute inset-x-0 flex items-center gap-2 px-3 font-mono text-xs"
              style={{ top: (start + i) * ROW, height: ROW }}
            >
              {f.encrypted && (
                <Lock
                  size={12}
                  className="shrink-0 text-[var(--color-muted)]"
                  aria-label={tr("viewer_files_encrypted", undefined, "Encrypted")}
                />
              )}
              <span className="min-w-0 flex-1 truncate" title={f.path}>
                {f.path}
              </span>
              <span className="shrink-0 tabular-nums text-[var(--color-muted)]">{formatBytes(f.size)}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

export function FilesTab({ token }: { token: string }) {
  const [files, setFiles] = useState<InspectFile[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [scrollTop, setScrollTop] = useState(0);

  useEffect(() => {
    let live = true;
    setFiles(null);
    setError(null);
    void gameInspectFiles(token)
      .then((r) => {
        if (!live) return;
        setFiles(r.files);
        setTruncated(r.truncated);
      })
      .catch((e) => live && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [token]);

  if (error) return <div className="text-sm text-[var(--color-bad)]">{error}</div>;
  if (!files) return <Spinner size={16} />;
  return (
    <FilesListView
      files={files}
      truncated={truncated}
      query={query}
      onQuery={(q) => {
        setQuery(q);
        setScrollTop(0);
      }}
      scrollTop={scrollTop}
      onScroll={setScrollTop}
    />
  );
}
