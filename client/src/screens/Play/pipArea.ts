// The Windows pop-out fix (#566, #575): what part of the popup's page its window shows.

/** The visible part of a popup's page, in its CSS pixels, from what the window itself says (its
 *  inner size in device pixels over the page's pixel ratio). */
export function visibleArea(
  physical: { width: number; height: number },
  dpr: number,
  page: { width: number; height: number },
): { width: number; height: number } | null {
  const r = dpr > 0 ? dpr : 1;
  const width = physical.width / r;
  const height = physical.height / r;
  // Only ever smaller: a page laid out bigger than its window shows just its top-left part.
  if (width >= page.width - 2 && height >= page.height - 2) return null;
  return {
    width: Math.min(width, page.width),
    height: Math.min(height, page.height),
  };
}
