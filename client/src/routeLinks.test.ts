// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { describe, expect, it } from "vitest";

// Collection's "Progress shows in Install Package" went to "/install", a page that does not
// exist, and the catch-all route quietly sent the user Home. Every fixed in-app link in the
// source must name a route App.tsx declares.

const src: string = new URL(".", import.meta.url).pathname;
const join = (a: string, b: string) => `${a.replace(/\/$/, "")}/${b}`;
const app: string = readFileSync(join(src, "App.tsx"), "utf8");
const routes = [...app.matchAll(/path="([^"]+)"/g)].map((m) => m[1]).filter((p) => p !== "*");

function files(dir: string): string[] {
  return (readdirSync(dir) as string[]).flatMap((n: string) => {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) return n === "i18n" ? [] : files(p);
    return /\.tsx?$/.test(n) && !/\.test\.tsx?$/.test(n) ? [p] : [];
  });
}

/** Folders on the PS5 itself: paths, not pages. */
const CONSOLE_ROOTS = new Set(["data", "user", "mnt", "system", "system_ex", "sce_sys", "preinst", "av_contents"]);

/** Whether `link` (no query or hash) is a route, `:param` segments matching anything. */
function isRoute(link: string): boolean {
  const parts = link.split("/");
  return routes.some((r) => {
    const rp = r.split("/");
    return rp.length === parts.length && rp.every((s: string, i: number) => s.startsWith(":") || s === parts[i]);
  });
}

describe("in-app links", () => {
  it("found the routes", () => {
    expect(routes.length).toBeGreaterThan(20);
  });

  it("all go to a page that exists", () => {
    const bad: string[] = [];
    const pattern = /(?:navigate\(|link:\s*|\bto=|href=|route:\s*|\?\s*)["'`](\/[A-Za-z0-9/_-]*)(?:[?#][^"'`]*)?["'`]/g;
    for (const f of files(src)) {
      const text: string = readFileSync(f, "utf8");
      for (const m of text.matchAll(pattern)) {
        const link = m[1].replace(/\/$/, "") || "/";
        if (link === "/" || link.startsWith("/api/") || CONSOLE_ROOTS.has(link.split("/")[1])) continue;
        if (!isRoute(link)) bad.push(`${f.slice(src.length)}: ${link}`);
      }
    }
    expect(bad).toEqual([]);
  });
});
