import { AlertTriangle, Disc3, FolderOpen, HardDrive, Package } from "lucide-react";

import type { InstalledTitle } from "../../api/ps5";
import { useTr } from "../../state/lang";

export type InstalledKind = "installed" | "disc" | "folder" | "system";

/** How a title got onto the console, in the user's terms:
 *   - installed: no source path (a .pkg install or a shipped Sony app)
 *   - disc:      a disk-image mount (/mnt/shadowmnt, needs ShadowMount+)
 *   - folder:    a game folder registered by us
 *   - system:    NPXS system title */
export function installedKindOf(t: InstalledTitle): InstalledKind {
  if (t.system) return "system";
  if (t.origin === "registered") {
    const src = t.source || "";
    if (t.imageBacked || src.startsWith("/mnt/shadowmnt")) return "disc";
    if (src.startsWith("/data/homebrew")) return "folder";
    // Registered with no (or some other) source path: a folder when there is one.
    return src ? "folder" : "installed";
  }
  return "installed";
}

/** "Disk image" / "Folder" / "Installed" / "System", with its icon. */
export function InstalledKindBadge({
  title,
  kraken = false,
}: {
  title: InstalledTitle;
  /** Kraken Mount lists this title: it mounts it from a package or folder, nothing installed. */
  kraken?: boolean;
}) {
  const tr = useTr();
  const k = installedKindOf(title);
  const base =
    "inline-flex shrink-0 items-center gap-1 whitespace-nowrap text-xs text-[var(--color-muted)]";
  if (kraken && k !== "system")
    return (
      <span className={base} data-testid="kraken-badge">
        <HardDrive size={11} />
        {tr("installed_badge_kraken", undefined, "Mounted by Kraken Mount")}
      </span>
    );
  if (k === "system")
    return (
      <span className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap text-xs font-medium text-[var(--color-bad)]">
        <AlertTriangle size={11} />
        {tr("installed_badge_system", undefined, "System")}
      </span>
    );
  if (k === "disc")
    return (
      <span className={base}>
        <Disc3 size={11} />
        {tr("installed_badge_image", undefined, "Disk image")}
      </span>
    );
  if (k === "folder")
    return (
      <span className={base}>
        <FolderOpen size={11} />
        {tr("installed_badge_folder", undefined, "Folder")}
      </span>
    );
  return (
    <span className={base}>
      <Package size={11} />
      {tr("installed_badge_installed", undefined, "Installed")}
    </span>
  );
}
