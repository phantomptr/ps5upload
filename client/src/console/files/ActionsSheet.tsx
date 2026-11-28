import {
  Binary,
  Download,
  FilePen,
  FilePlus,
  ClipboardCopy,
  FileArchive,
  PackageOpen,
  FileCheck,
  Replace,
  Copy,
  Eye,
  FolderInput,
  FolderOpen,
  FolderPlus,
  Info,
  PackagePlus,
  PencilLine,
  Rocket,
  Scissors,
  ShieldCheck,
  Star,
  StarOff,
  Trash2,
  type LucideIcon, ListPlus } from "lucide-react";
import { t } from "../strings";
import { Modal } from "../ui/Modal";
import { SHOW_ON_PC_ITEM } from "./handoff";

export type SheetAction =
  | "open"
  | "view"
  | "clipcopy"
  | "cut"
  | "copy"
  | "move"
  | "rename"
  | "bulkrename"
  | "mkdir"
  | "newfile"
  | "edit"
  | "download"
  | "info"
  | "pin"
  | "unpin"
  | "verify"
  | "hex"
  | "unpack"
  | "install"
  /** Add to the console's queue (#982): a package's install, or a copy from a share. */
  | "queue"
  | "loadpayload"
  | "extract"
  | "showpc"
  | "fixperm"
  | "delete"
  /** A saved network share's menu (the drive list). */
  | "shareedit"
  | "shareremove";

const ITEMS: Record<SheetAction, { icon: LucideIcon; key: string }> = {
  open: { icon: FolderOpen, key: "cw_files_open" },
  view: { icon: Eye, key: "cw_files_view" },
  clipcopy: { icon: ClipboardCopy, key: "cw_files_clip_copy" },
  cut: { icon: Scissors, key: "cw_files_clip_cut" },
  copy: { icon: Copy, key: "cw_files_copy_to" },
  move: { icon: FolderInput, key: "cw_files_move_to" },
  rename: { icon: PencilLine, key: "cw_files_rename" },
  bulkrename: { icon: Replace, key: "cw_files_bulk_rename" },
  mkdir: { icon: FolderPlus, key: "cw_files_new_folder" },
  newfile: { icon: FilePlus, key: "cw_files_new_file" },
  edit: { icon: FilePen, key: "cw_files_edit_as_text" },
  download: { icon: Download, key: "cw_files_download" },
  info: { icon: Info, key: "cw_files_info" },
  pin: { icon: Star, key: "cw_files_pin" },
  unpin: { icon: StarOff, key: "cw_files_unpin" },
  verify: { icon: FileCheck, key: "cw_files_verify" },
  hex: { icon: Binary, key: "cw_files_vw_as_hex" },
  unpack: { icon: FileArchive, key: "cw_files_unpack" },
  install: { icon: PackagePlus, key: "cw_pkg_install" },
  queue: { icon: ListPlus, key: "cw_queue_add" },
  loadpayload: { icon: Rocket, key: "cw_payload_load" },
  extract: { icon: PackageOpen, key: "cw_files_extract" },
  showpc: SHOW_ON_PC_ITEM,
  fixperm: { icon: ShieldCheck, key: "cw_files_fixperm" },
  delete: { icon: Trash2, key: "cw_files_delete" },
  shareedit: { icon: PencilLine, key: "cw_files_smb_edit" },
  shareremove: { icon: Trash2, key: "cw_files_smb_remove" },
};

/** The ones a running copy/move/delete request holds back (one batch at a time). */
const HELD: ReadonlySet<SheetAction> = new Set(["copy", "move", "bulkrename", "unpack", "install", "extract", "fixperm", "delete"]);

/** More actions than this go in two columns. */
export const SHEET_ONE_COLUMN = 8;

/**
 * △ / "⋯": what can be done with one item or the selection, as a list of
 * buttons (D-pad + ✕). ○ or △ closes it.
 */
export function ActionsSheet({
  title,
  actions,
  busy,
  note,
  onChoose,
  onClose,
}: {
  title: string;
  actions: SheetAction[];
  /** A copy/move/delete request is running: those are off until it answers. */
  busy: boolean;
  /** The running request's progress line. */
  note?: string | null;
  onChoose: (a: SheetAction) => void;
  onClose: () => void;
}) {
  // A long list (a file's full menu) in two columns, top to bottom: it fits the TV's 700 px height.
  const two = actions.length > SHEET_ONE_COLUMN;
  return (
    <Modal label={title} className={`cw-sheet${two ? " is-two" : ""}`} onBack={onClose}>
      <h2 className="cw-sheet-title">{title}</h2>
      {note ? (
        <p className="cw-dim" role="status">
          {note}
        </p>
      ) : null}
      <div
        className="cw-sheet-items"
        style={two ? { gridTemplateRows: `repeat(${Math.ceil(actions.length / 2)}, auto)` } : undefined}
      >
        {actions.map((a, i) => {
          const { icon: Icon, key } = ITEMS[a];
          return (
            <button
              key={a}
              type="button"
              className={`cw-focusable cw-sheet-item${a === "delete" || a === "shareremove" ? " is-danger" : ""}`}
              data-sheet={a}
              data-nav-default={i === 0 ? "" : undefined}
              disabled={busy && HELD.has(a)}
              onClick={() => onChoose(a)}
            >
              <Icon aria-hidden />
              <span>{t(key)}</span>
            </button>
          );
        })}
      </div>
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" onClick={onClose}>
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
    </Modal>
  );
}
