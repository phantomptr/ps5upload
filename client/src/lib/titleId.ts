// Title-id helpers.

/** The title id whose icon a folder name stands for: `PPSA17221` itself, the leading id of a
 *  suffixed copy (`PPSA17221.bak`, `CUSA07842_00`), or null for a name that isn't one (the
 *  engine answers 400 to anything that isn't a bare title id). */
export function iconTitleId(name: string): string | null {
  const m = name.trim().match(/^([A-Z]{4}\d{5})(?![0-9A-Z])/i);
  return m ? m[1].toUpperCase() : null;
}
