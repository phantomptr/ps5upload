import {
  Archive,
  Disc3,
  File,
  FileCode2,
  FileImage,
  FileText,
  FileVideo,
  Folder,
  Link2,
  Package,
  type LucideIcon,
} from "lucide-react";
import { formatBytes } from "../../lib/format";
import { t } from "../strings";
import { ActionsButton } from "../ui/ActionsButton";
import { iconKind, type IconKind } from "./listing";
import type { Entry } from "./useDir";

/** What the viewer can show: images and text (others are not rendered). */
export function viewKind(name: string): "image" | "text" | null {
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return null;
  const ext = name.slice(dot + 1).toLowerCase();
  if (ext === "png" || ext === "jpg" || ext === "jpeg") return "image";
  if (ext === "txt" || ext === "log" || ext === "json" || ext === "ini" || ext === "xml") return "text";
  return null;
}

const ICONS: Record<IconKind, LucideIcon> = {
  folder: Folder,
  link: Link2,
  image: FileImage,
  text: FileText,
  disk: Disc3,
  package: Package,
  exec: FileCode2,
  archive: Archive,
  video: FileVideo,
  other: File,
};

/** Rows drawn at first, and added per "Show more rows" (a 20,000-row folder keeps a small DOM). */
export const ROW_WINDOW = 300;

/**
 * A folder's entries, one row each: ✕ opens (a folder, a viewable file) or,
 * in select mode, marks the row; "⋯" (or △) opens its actions. Names are text
 * nodes only, so any name (quotes, CJK, markup) shows as it is.
 */
export function DirList({
  entries,
  selecting,
  selected,
  onEntry,
  onToggleItem,
  onActions,
  label,
  limit = Infinity,
  onMoreRows,
  uploading,
  revealed,
}: {
  entries: Entry[];
  selecting: boolean;
  selected: string[];
  onEntry: (e: Entry) => void;
  onToggleItem: (name: string) => void;
  onActions: (name: string) => void;
  label?: string;
  /** Draw only the first `limit` rows; the rest wait behind "Show more rows". */
  limit?: number;
  onMoreRows?: () => void;
  /** Entries a running PC upload is writing into: an "Uploading" badge. */
  uploading?: ReadonlySet<string>;
  /** The folder just come back out of: lit for a moment (lib/revealChild.ts). */
  revealed?: string | null;
}) {
  const marked = new Set(selected);
  const drawn = entries.length > limit ? entries.slice(0, limit) : entries;
  return (
    <ul className="cw-files" data-nav-list aria-label={label}>
      {drawn.map((e) => {
        const Icon = ICONS[iconKind(e)];
        const on = marked.has(e.name);
        return (
          <li
            key={e.name}
            className={`cw-file${on ? " is-selected" : ""}${revealed === e.name ? " is-revealed" : ""}`}
            data-nav-item
            data-entry={e.name}
          >
            <button
              type="button"
              className="cw-focusable cw-file-main"
              data-entry-main=""
              aria-pressed={selecting ? on : undefined}
              onClick={() => (selecting ? onToggleItem(e.name) : onEntry(e))}
            >
              <Icon className="cw-file-icon" aria-hidden />
              <span className="cw-file-text">
                <span className="cw-file-name">{e.name}</span>
                {uploading?.has(e.name) ? (
                  <span className="cw-tag is-pc" data-uploading="">
                    {t("cw_files_uploading")}
                  </span>
                ) : null}
              </span>
              <span className="cw-file-meta cw-dim">
                {e.kind === "dir" ? t("cw_files_kind_dir") : formatBytes(e.size)}
              </span>
            </button>
            <ActionsButton onPress={() => onActions(e.name)} />
          </li>
        );
      })}
      {drawn.length < entries.length && onMoreRows ? (
        <li className="cw-file is-plain" data-nav-item>
          <button
            type="button"
            className="cw-focusable cw-file-main"
            data-action="more-rows"
            onFocus={onMoreRows}
            onClick={onMoreRows}
          >
            <span className="cw-file-text">
              <span className="cw-file-name">{t("cw_files_rows_more", { n: entries.length - drawn.length })}</span>
            </span>
          </button>
        </li>
      ) : null}
    </ul>
  );
}
