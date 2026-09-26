// Pure path helper for the Screenshots "Convert" action.
//
// A PS5 screenshot arrives as either the full-res original `<name>.jxr`
// or Sony's doubled-suffix thumbnail `<name>.jxr.jxr`. Converting it to a
// viewable PNG means producing `<name>.png` in both cases. Extracted as a
// pure function so the stem rule is unit-testable without the React screen
// or a Tauri runtime. Mirrors the payload's `ss_stem` (runtime.c): strip
// trailing image extensions repeatedly so `.jxr` and `.jxr.jxr` collapse
// to the same stem.

/** Filename for the converted PNG given a screenshot's basename.
 *  `NAME.jxr` → `NAME.png`; `NAME.jxr.jxr` → `NAME.png`;
 *  `NAME.jpg` → `NAME.png`. A name with no known image extension just
 *  gets `.png` appended (defensive — shouldn't happen for listed shots). */
export function pngNameForJxr(name: string): string {
  let stem = name;
  // Repeatedly peel a trailing image extension (case-insensitive).
  // The loop handles the doubled `.jxr.jxr` thumbnail suffix.
  for (;;) {
    const stripped = stem.replace(/\.(jxr|jpg|jpeg)$/i, "");
    if (stripped === stem) break;
    stem = stripped;
  }
  return `${stem}.png`;
}

/** Whether a screenshot needs JPEG-XR → PNG conversion to be viewable.
 *  PS5 HDR captures are `.jxr` (including the doubled `.jxr.jxr`
 *  thumbnail); SDR captures arrive as `.jpg`/`.jpeg`/`.png`, which the
 *  WebView renders directly — running the jxr decoder on those fails with
 *  "not a JPEG XR file". Callers should only convert when this is true. */
export function isJxrScreenshot(name: string): boolean {
  return /\.jxr$/i.test(name);
}

/** Join a directory and a filename with a forward slash. Rust's `Path`
 *  (and the Windows APIs underneath it) accept `/` as a separator on
 *  every desktop target, so a single join works cross-platform even when
 *  `dir` came from a native picker using backslashes. */
export function joinDir(dir: string, name: string): string {
  const trimmed = dir.replace(/[\\/]+$/g, "");
  return `${trimmed}/${name}`;
}
