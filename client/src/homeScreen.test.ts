// A phone's "Add to Home Screen" on every page PS5Upload serves: the name "PS5Upload", the icon
// and a manifest (engine app_icon.rs; the console page's from the helper, webui_offline.h).
// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (rel: string): string => readFileSync(new URL(rel, import.meta.url), "utf8");

const pages: [string, string, string, string][] = [
  // [file, touch icon, manifest, label]
  ["../index.html", "/app-icon/apple-touch-icon.png", "/app-icon/app.webmanifest", "web UI"],
  ["./join/index.html", "../app-icon/apple-touch-icon.png", "../app-icon/page.webmanifest", "join"],
  ["./pad/index.html", "../app-icon/apple-touch-icon.png", "../app-icon/page.webmanifest", "pad"],
  ["../../engine/crates/ps5upload-engine/src/join_page.html", "../app-icon/apple-touch-icon.png", "../app-icon/page.webmanifest", "built join"],
  ["../../engine/crates/ps5upload-engine/src/pad_page.html", "../app-icon/apple-touch-icon.png", "../app-icon/page.webmanifest", "built pad"],
  ["./console/index.html", "/apple-touch-icon.png", "/manifest.webmanifest", "console"],
];

describe("Home Screen name and icon", () => {
  for (const [file, icon, manifest, label] of pages) {
    it(label, () => {
      const html = read(file);
      const head = html.slice(0, html.indexOf("</head>"));
      expect(head).toContain('<meta name="apple-mobile-web-app-title" content="PS5Upload"');
      expect(head.match(/apple-mobile-web-app-title/g)).toHaveLength(1);
      expect(head).toContain(`<link rel="apple-touch-icon" href="${icon}"`);
      expect(head).toContain(`<link rel="manifest" href="${manifest}"`);
    });
  }

  it("the manifests name PS5Upload", () => {
    for (const f of ["app.webmanifest", "page.webmanifest"]) {
      const m = JSON.parse(read(`../../engine/crates/ps5upload-engine/src/app_icon/${f}`));
      expect(m.name).toBe("PS5Upload");
      expect(m.short_name).toBe("PS5Upload");
    }
  });
});
