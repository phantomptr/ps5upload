import { describe, expect, it } from "vitest";
import { classUnused, packStrings, pruneUnusedClasses, stillNamed } from "./vite.pagePlugins";
import { unpack } from "./src/join/unpackStrings";

describe("guest page strings", () => {
  it("are packed with the keys once and unpack to what each language says", () => {
    const locales = {
      en: { a: "Hello", b: "Bye", c: "Room" },
      de: { a: "Hallo", b: "Bye", c: "Raum" },
      fr: { a: "Bonjour" },
    };
    const p = packStrings(locales, ["a", "b", "c"]);
    expect(p.k).toEqual(["a", "b", "c"]);
    // the same as English (or nothing) is 0: English is the fallback
    expect(p.l.de).toEqual(["Hallo", 0, "Raum"]);
    expect(p.l.fr).toEqual(["Bonjour", 0, 0]);
    expect(unpack(p, "de")).toEqual({ a: "Hallo", c: "Raum" });
    expect(unpack(p, "en")).toEqual(locales.en);
    expect(unpack(p, "xx")).toEqual({});
  });

  it("keep only keys the built code still names", () => {
    const code = 'tr(`join_ok`);x("room_a");y=\'room_b\'';
    expect(stillNamed(code, "join_ok")).toBe(true);
    expect(stillNamed(code, "room_a")).toBe(true);
    expect(stillNamed(code, "room_b")).toBe(true);
    expect(stillNamed(code, "join")).toBe(false);
    expect(stillNamed(code, "usb_skipped_unknown")).toBe(false);
  });
});

describe("guest page stylesheet", () => {
  const code = 'className:"card px-2",b=`lucide-${n}`,c="hover:bg-white/10"';
  it("knows a class the page never names, and one it may build", () => {
    expect(classUnused(code, "card")).toBe(false);
    expect(classUnused(code, "anim-sheet-up")).toBe(true);
    expect(classUnused(code, "lucide-x")).toBe(false);
    expect(classUnused(code, "hover:bg-white/10")).toBe(false);
  });

  it("drops only rules for classes named nowhere", () => {
    const css =
      ".card{color:red}.anim-sheet-up:not(#\\#):not(#\\#){animation:x}" +
      ".card,.support-heart{margin:0}.btn:not(.support-heart){padding:0}" +
      "@media (prefers-reduced-motion:reduce){.anim-sheet-up{animation:none}}" +
      ".hover\\:bg-white\\/10:hover{color:blue}@keyframes k{from{opacity:0}}";
    const out = pruneUnusedClasses(css, code);
    expect(out).toContain(".card{color:red}");
    expect(out).not.toContain("anim-sheet-up");
    expect(out).not.toContain("@media");
    expect(out).toContain(".card{margin:0}");
    expect(out).toContain(".btn:not(.support-heart){padding:0}");
    expect(out).toContain(".hover\\:bg-white\\/10:hover");
    expect(out).toContain("@keyframes k");
  });
});
