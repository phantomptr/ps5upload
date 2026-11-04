import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";

import { pageFiles, scopedCss } from "./vite.pageCss";

describe("pageFiles", () => {
  it("walks relative imports, follows a swap, and leaves packages and styles out", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "page-css-"));
    mkdirSync(path.join(dir, "join"));
    mkdirSync(path.join(dir, "components"));
    const put = (name: string, text: string) =>
      writeFileSync(path.join(dir, name), text);
    put(
      "join/main.tsx",
      'import "../index.css";\nimport { A } from "./A";\nimport x from "react";\n',
    );
    put(
      "join/A.tsx",
      'export { B } from "../components";\nconst l = () => import("./Lazy");\nimport "../lang";\n',
    );
    put("join/Lazy.tsx", "export {};\n");
    put("components/index.ts", "export const B = 1;\n");
    put("index.css", "");
    put("lang.ts", "// the real one\n");
    put("join/langStub.ts", "// the page's own\n");
    const swap = (f: string) =>
      f.endsWith("/lang.ts") ? path.join(dir, "join/langStub.ts") : f;
    const rel = pageFiles(path.join(dir, "join/main.tsx"), swap).map((f) =>
      path.relative(dir, f),
    );
    expect(rel).toEqual([
      "components/index.ts",
      "join/A.tsx",
      "join/Lazy.tsx",
      "join/langStub.ts",
      "join/main.tsx",
    ]);
  });
});

describe("scopedCss", () => {
  it("turns the import into a scan of those files, and leaves the rest of the sheet", () => {
    const out = scopedCss(
      '@import "tailwindcss";\n:root { --x: 1; }\n',
      "/c/src/index.css",
      ["/c/src/join/main.tsx", "/c/src/screens/Play/TouchControls.tsx"],
    );
    expect(out).toBe(
      '@import "tailwindcss" source(none);\n@source "join/main.tsx";\n@source "screens/Play/TouchControls.tsx";\n:root { --x: 1; }\n',
    );
  });
});
