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
import {
  ExtractDialog,
  extractChoice,
  extractDest,
  extractHereAllowed,
  hereChoice,
  hereConflicts,
  listErrorKind,
  namesAt,
  type ExtractChoice,
} from "./ExtractDialog";

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

describe("Extract here (#891)", () => {
  const taken = (names: string[]) => (n: string) => names.includes(n);
  const listing = { top: ["Game", "readme.txt", "fresh.txt"], dirs: ["Game"], more: false };

  it("writes into the folder itself, never a drive's top folder", () => {
    expect(extractDest({ parent: "/mnt/usb0/games/", folder: "", here: true })).toBe("/mnt/usb0/games");
    expect(extractHereAllowed("/mnt/usb0/games")).toBe(true);
    expect(extractHereAllowed("/data/homebrew")).toBe(true);
    expect(extractHereAllowed("/data")).toBe(false);
    expect(extractHereAllowed("/mnt/usb0")).toBe(false);
    expect(extractHereAllowed("/mnt/ext1/")).toBe(false);
  });

  it("finds the archive's top-level items already in the folder", () => {
    expect(hereConflicts(listing.top, taken(["Game", "readme.txt", "other"]))).toEqual(["Game", "readme.txt"]);
    expect(hereConflicts(listing.top, taken([]))).toEqual([]);
  });

  it("without conflicts merges in and never replaces", () => {
    expect(hereChoice("/data/x", listing, [], null, taken([]), "pw")).toEqual({
      parent: "/data/x",
      folder: "",
      here: true,
      merge: true,
      replace: false,
      password: "pw",
    });
  });

  it("Replace overwrites, Skip existing leaves those items out", () => {
    const exists = taken(["Game", "readme.txt"]);
    const conflicts = ["Game", "readme.txt"];
    const r = hereChoice("/data/x", listing, conflicts, "replace", exists, "");
    expect(r.replace).toBe(true);
    expect(r.top).toBeUndefined();
    const s = hereChoice("/data/x", listing, conflicts, "skip", exists, "");
    expect(s.replace).toBe(false);
    expect(s.top).toEqual([
      { from: "Game", to: "" },
      { from: "readme.txt", to: "" },
    ]);
  });

  it("Keep both renames the incoming item to a name free in the folder and in the archive", () => {
    const exists = taken(["Game", "Game (2)", "readme.txt"]);
    const withTwo = { ...listing, top: [...listing.top, "readme (2).txt"] };
    const k = hereChoice("/data/x", withTwo, ["Game", "readme.txt"], "keep", exists, "");
    expect(k.replace).toBe(false);
    expect(k.merge).toBe(true);
    expect(k.top).toEqual([
      { from: "Game", to: "Game (3)" },
      { from: "readme.txt", to: "readme (3).txt" },
    ]);
    // a folder's dots are part of its name
    const dotted = { top: ["Game 1.05"], dirs: ["Game 1.05"], more: false };
    const v = hereChoice("/data/x", dotted, ["Game 1.05"], "keep", taken(["Game 1.05"]), "");
    expect(v.top).toEqual([{ from: "Game 1.05", to: "Game 1.05 (2)" }]);
  });

  it("tells a password, an older helper and other failures apart", () => {
    expect(listErrorKind("fs_extract_password_required")).toBe("password");
    expect(listErrorKind("fs_extract_wrong_password")).toBe("wrong_password");
    expect(listErrorKind("fs_extract_list_unsupported")).toBe("old_helper");
    expect(listErrorKind("payload rejected ARCHIVE_EXTRACT: fs_extract_destination_exists")).toBe("old_helper");
    expect(listErrorKind("fs_extract_corrupt")).toBe("other");
  });

  const render = (dir: string, initial: ExtractChoice | null) =>
    renderToStaticMarkup(
      <ExtractDialog
        open
        name="g.zip"
        dir={dir}
        exists={() => false}
        initial={initial}
        listTop={() => new Promise(() => undefined)}
        onCancel={vi.fn()}
        onConfirm={vi.fn()}
      />,
    );

  it("defaults to a new folder", () => {
    const html = render("/data/games", null);
    expect(html).toContain("Into a new folder");
    expect(html).toContain("Extract here");
    expect(html).toContain('data-testid="fs-extract-folder"');
  });

  it("Extract here shows no folder name and is refused at a drive's top folder", () => {
    const here: ExtractChoice = { parent: "/data/games", folder: "", here: true, merge: true, replace: false, password: "" };
    let html = render("/data/games", here);
    expect(html).not.toContain('data-testid="fs-extract-folder"');
    expect(html).toContain("straight into this folder");
    expect(html).not.toContain('data-testid="fs-extract-here-root"');
    expect(goButton(html)).not.toContain('disabled=""');
    html = render("/data", { ...here, parent: "/data" });
    expect(html).toContain('data-testid="fs-extract-here-root"');
    expect(goButton(html)).toContain('disabled=""');
  });
});
