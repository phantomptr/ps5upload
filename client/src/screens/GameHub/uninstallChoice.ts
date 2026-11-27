import type { InstalledTitle, UninstallSource } from "../../api/ps5";
import type { ConfirmChoice } from "../../components/ConfirmDialog";
import { formatBytes } from "../../lib/format";
import type { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

/** Whether Uninstall first asks the engine what the game plays from: only a title we registered
 *  from a folder or a disk image. A package install (the console's installer removes its own copy),
 *  a system title, and a ShadowMount+ or Kraken Mount title (they have their own wording) do not. */
export function asksAboutSource(t: InstalledTitle, opts: { kraken: boolean; smp: boolean }): boolean {
  return !t.system && !opts.kraken && !opts.smp && t.origin === "registered" && !!t.source;
}

/** "the folder (12.3 GiB)" detail line of the delete choice: the path and what it frees. */
export function sourceDetail(s: UninstallSource, tr: Tr): string {
  const path = s.path ?? "";
  if (typeof s.bytes !== "number") return path;
  const size = s.size_partial
    ? tr("installed_uninstall_size_at_least", { size: formatBytes(s.bytes) }, "at least {size}")
    : formatBytes(s.bytes);
  return `${path} (${size})`;
}

/** The two choices of an Uninstall that can also delete its source, "Uninstall only" first (the
 *  default); null when there is nothing to ask (the source is gone, or not safe to delete). */
export function uninstallChoices(s: UninstallSource, tr: Tr): ConfirmChoice[] | null {
  if (!s.offer || !s.path || (s.kind !== "folder" && s.kind !== "image")) return null;
  return [
    {
      value: "keep",
      label: tr("installed_uninstall_only", undefined, "Uninstall only"),
      detail: tr("installed_uninstall_keep_detail", { path: s.path }, "Keep {path}"),
    },
    {
      value: "delete",
      label:
        s.kind === "folder"
          ? tr("installed_uninstall_delete_folder", undefined, "Uninstall and delete the game folder")
          : tr("installed_uninstall_delete_image", undefined, "Uninstall and delete the disk image"),
      detail: sourceDetail(s, tr),
    },
  ];
}
