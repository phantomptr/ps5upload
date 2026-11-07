/** A backup file's date and time, local, sortable: `20261014-2035`. */
export function backupStamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}

/** Where Save as a file suggests a backup: in the save backup folder when one is set (and is on
 *  this computer), else just the name (the dialog's own default folder). */
export function inBackupFolder(dir: string | undefined | null, name: string): string {
  const d = (dir ?? "").trim().replace(/[\\/]+$/, "");
  if (!d) return name;
  return `${d}${d.includes("\\") && !d.includes("/") ? "\\" : "/"}${name}`;
}
