// The save images of a save folder, as Open save offers them.
import type { SaveEntry } from "../../api/ps5";
import { isConsoleBackupImage } from "../../api/saveShare";

/** The images in a save folder's listing, as full paths: its files without the `.bin` keys and
 *  the console's own backup copies (`sdimg_sce_bu_*`); a PS4 save's `sdimg_` files only. */
export function saveImagesOf(
  entry: Pick<SaveEntry, "path" | "kind">,
  children: ReadonlyArray<{ name: string; kind: string }>,
): string[] {
  const base = entry.path.endsWith("/") ? entry.path : `${entry.path}/`;
  return children
    .filter(
      (c) =>
        c.kind === "file" &&
        !/\.bin$/i.test(c.name) &&
        !c.name.startsWith(".") &&
        !isConsoleBackupImage(c.name) &&
        (entry.kind !== "ps4" || c.name.startsWith("sdimg_")),
    )
    .map((c) => `${base}${c.name}`);
}
