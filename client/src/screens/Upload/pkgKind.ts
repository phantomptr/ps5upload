// The Upload screen's "Detected:" label for a .pkg, by the console its header names.

type Tr = (key: string, fallback: string) => string;

/** "PS4 package (.pkg)" / "PS5 package (.pkg)", or a plain "Package (.pkg)" when the header
 *  names no console — never a guess. */
export function pkgKindLabel(platform: string | null | undefined, tr: Tr): string {
  switch (platform) {
    case "ps4":
      return tr("upload_kind_pkg_ps4", "PS4 package (.pkg)");
    case "ps5":
      return tr("upload_kind_pkg", "PS5 package (.pkg)");
    default:
      return tr("upload_kind_pkg_unknown", "Package (.pkg)");
  }
}
