// Which file of a multi-part RAR set stands for the whole set.

/** True for the first (or only) volume of a RAR set: `.rar`, `.part1.rar`, `.part01.rar`. */
export function isRarFirstVolume(path: string): boolean {
  const name = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const lower = name.toLowerCase();
  if (!lower.endsWith(".rar")) return false;
  const part = /\.part0*(\d+)\.rar$/.exec(lower);
  return part ? Number(part[1]) === 1 : true;
}

/** True for `.part2.rar` and later: read through the first part, never a game of its own. */
export function isRarLaterVolume(path: string): boolean {
  return /\.rar$/i.test(path.trim()) && !isRarFirstVolume(path.trim());
}
