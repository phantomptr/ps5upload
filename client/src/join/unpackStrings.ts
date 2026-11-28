// The guest pages' strings come packed (vite.pagePlugins.ts packStrings): the keys once, then each
// language's texts in that order, 0 where English's is used. One language's texts by key.

export function unpack(p: { k: string[]; l: Record<string, (string | 0)[]> }, code: string): Record<string, string> {
  const out: Record<string, string> = {};
  const texts = p.l[code] ?? [];
  p.k.forEach((k, i) => {
    const t = texts[i];
    if (typeof t === "string") out[k] = t;
  });
  return out;
}
