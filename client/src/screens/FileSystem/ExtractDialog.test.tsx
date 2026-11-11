import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import type { Volume } from "../../api/ps5";
import { ExtractDialog, extractChoice, extractDest, namesAt, type ExtractChoice } from "./ExtractDialog";

describe("extractChoice", () => {
  const taken = (names: string[]) => (n: string) => names.includes(n);

  it("a free folder is used as it is, never merged", () => {
    expect(extractChoice("/data/x", "g", "merge", taken([]), "")).toEqual({ parent: "/data/x", folder: "g", merge: false, replace: false, password: "" });
  });

  it("Keep both picks the next free name", () => {
    expect(extractChoice("/data/x", "g", "new", taken(["g", "g (2)"]), "pw")).toEqual({ parent: "/data/x", folder: "g (3)", merge: false, replace: false, password: "pw" });
  });

  it("Merge writes into the folder already there, skipping or replacing", () => {
    expect(extractChoice("/data/x", "g", "merge", taken(["g"]), "")).toEqual({ parent: "/data/x", folder: "g", merge: true, replace: false, password: "" });
    expect(extractChoice("/data/x", "g", "replace", taken(["g"]), "")).toEqual({ parent: "/data/x", folder: "g", merge: true, replace: true, password: "" });
  });

  it("keeps the location picked, on another drive too", () => {
    expect(extractChoice("/mnt/ext0/games", "g", "new", taken(["g"]), "").parent).toBe("/mnt/ext0/games");
  });
});

describe("extractDest", () => {
  it("is the new folder inside the location picked", () => {
    expect(extractDest({ parent: "/mnt/usb0/archives", folder: "g" })).toBe("/mnt/usb0/archives/g");
    expect(extractDest({ parent: "/data", folder: "g" })).toBe("/data/g");
    expect(extractDest({ parent: "/data/", folder: "g" })).toBe("/data/g");
  });
});

describe("namesAt: a folder already there in another location", () => {
  const here = (n: string) => n === "here";

  it("the archive's folder uses the listing shown", () => {
    expect(namesAt("/data/x", "/data/x/", here, null)).toBe(here);
  });

  it("another folder waits for its names, then checks them", () => {
    expect(namesAt("/data", "/mnt/ext0/x", here, null)).toBeNull();
    const at = namesAt("/data", "/mnt/ext0/x", here, ["g", "other"]);
    expect(at?.("g")).toBe(true);
    expect(at?.("here")).toBe(false);
  });

  it("without a listing shown, the archive's folder is read too", () => {
    expect(namesAt("/data/x", "/data/x", undefined, null)).toBeNull();
    expect(namesAt("/data/x", "/data/x", undefined, ["g"])?.("g")).toBe(true);
  });
});

const goButton = (html: string) => html.match(/<button[^>]*data-testid="fs-extract-go"[^>]*>/)?.[0] ?? "";

describe("ExtractDialog: Extract to", () => {
  const vol = (path: string, free: number): Volume => ({ path, fs_type: "exfat", total_bytes: 10_000, free_bytes: free, writable: true });
  const VOLUMES = [vol("/data", 100), vol("/mnt/ext0", 9_000)];
  const render = (initial: ExtractChoice | null = null) =>
    renderToStaticMarkup(
      <ExtractDialog
        open
        name="g.zip"
        dir="/mnt/ext0/archives"
        exists={(n) => n === "taken"}
        host="192.0.2.1"
        addr="192.0.2.1:9113"
        volumes={VOLUMES}
        listNames={() => new Promise(() => undefined)}
        initial={initial}
        archiveBytes={500}
        onCancel={vi.fn()}
        onConfirm={vi.fn()}
      />,
    );

  it("starts beside the archive, with a Change button and every drive as a quick pick", () => {
    const html = render();
    expect(html).toContain("Extract to");
    expect(html).toMatch(/data-testid="fs-extract-parent"[^>]*>\/mnt\/ext0\/archives</);
    expect(html).toContain("/mnt/ext0/archives/g");
    expect(html).toContain('data-testid="fs-extract-change"');
    expect(html).toContain(">/data<");
    expect(html).toContain(">/mnt/ext0<");
    expect(html).not.toContain('data-testid="fs-extract-beside"');
    // The archive's drive has room for it.
    expect(html).not.toContain('data-testid="fs-extract-room"');
    expect(goButton(html)).not.toContain('disabled=""');
  });

  it("another location: writes there, checks that folder before Extract, and warns on its free space", () => {
    const html = render({ parent: "/data", folder: "g", merge: false, replace: false, password: "" });
    expect(html).toMatch(/data-testid="fs-extract-parent"[^>]*>\/data</);
    expect(html).toContain("/data/g");
    expect(html).toContain('data-testid="fs-extract-beside"');
    expect(html).toContain("Loading…");
    // The names there are not known yet: no start that could clash.
    expect(goButton(html)).toContain('disabled=""');
    // /data has 100 bytes free, less than the 500-byte archive.
    expect(html).toContain('data-testid="fs-extract-room"');
  });
});
