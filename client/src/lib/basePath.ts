/**
 * Where the engine hosts this page: "" at the root, "/ps5" below a path.
 *
 * The prefix (PS5UPLOAD_BASE_URL) is set when the container starts, on the one published image, so
 * the bundle cannot know it: the engine writes it into the page it serves. A build with a prefix
 * baked in (VITE_BASE_URL) still works, and neither exists in the desktop and phone apps.
 */
export function pageBasePath(): string {
  if (typeof document !== "undefined") {
    const meta = document.querySelector('meta[name="ps5upload-base"]');
    const written = meta?.getAttribute("content");
    if (written !== null && written !== undefined)
      return written.replace(/\/+$/, "");
  }
  return (import.meta.env.VITE_BASE_URL || "")
    .replace(/^\.?\/+$/, "")
    .replace(/\/+$/, "");
}
