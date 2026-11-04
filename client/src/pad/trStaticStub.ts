// lib/trStatic for the pad page (vite.pad.config.ts): English only, like langStub.

export function trStatic(
  _key: string,
  fallback: string,
  vars?: Record<string, string | number>,
): string {
  if (!vars) return fallback;
  return fallback.replace(/\{(\w+)\}/g, (all, name: string) =>
    name in vars ? String(vars[name]) : all,
  );
}
