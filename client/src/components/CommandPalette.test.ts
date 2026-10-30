import { afterEach, describe, expect, it, vi } from "vitest";

import { OPEN_COMMAND_PALETTE_EVENT, openCommandPalette, paletteGames, paletteScreens } from "./CommandPalette";
import type { Translator } from "../state/lang";

describe("openCommandPalette", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("sends the event the palette listens for", () => {
    const target = new EventTarget();
    vi.stubGlobal("window", target);
    const seen: string[] = [];
    target.addEventListener(OPEN_COMMAND_PALETTE_EVENT, (e) => seen.push(e.type));
    openCommandPalette();
    expect(seen).toEqual([OPEN_COMMAND_PALETTE_EVENT]);
  });
});

describe("the palette's screens", () => {
  const where = { betaEnabled: false, inBrowser: false, macOrLinux: true };

  it("lists Home, Play and Bug report, then pinned screens in pin order, then the rest", () => {
    const order = paletteScreens(["/payloads", "/files"], where).map((i) => i.to);
    expect(order.slice(0, 5)).toEqual(["/home", "/play", "/bug-report", "/payloads", "/files"]);
    // Each screen once.
    expect(new Set(order).size).toBe(order.length);
    const plain = paletteScreens([], where).map((i) => i.to);
    expect([...order].sort()).toEqual([...plain].sort());
  });

  it("keeps the usual order with nothing pinned, and skips pins this build can't show", () => {
    const plain = paletteScreens([], where).map((i) => i.to);
    expect(plain.slice(0, 3)).toEqual(["/home", "/play", "/bug-report"]);
    // Shell has no browser path: pinned or not, the browser build leaves it out.
    const browser = paletteScreens(["/shell", "/files"], { ...where, inBrowser: true }).map((i) => i.to);
    expect(browser).not.toContain("/shell");
    expect(browser[3]).toBe("/files");
  });
});


describe("the palette's games", () => {
  const tr = ((_k: string, vars: Record<string, string>, fallback: string) =>
    fallback.replace("{consoles}", vars.consoles)) as unknown as Translator;
  const label = (h: string) => (h === "10.0.0.2" ? "Living room" : h);

  it("adds games only a console has, and says where each is installed", () => {
    const got = paletteGames(
      [
        { game_id: "PPSA01234", title: "Astro" },
        { game_id: "CUSA00001", title: "On a drive" },
      ],
      [
        { title_id: "PPSA01234", title: "Astro Bot", hosts: ["10.0.0.2", "10.0.0.3"] },
        { title_id: "PPSA09999", title: "", hosts: ["10.0.0.3"] },
      ],
      label,
      tr,
    );
    expect(got).toEqual([
      { id: "PPSA01234", title: "Astro", hint: "Installed on Living room, 10.0.0.3" },
      { id: "CUSA00001", title: "On a drive", hint: "CUSA00001" },
      { id: "PPSA09999", title: "PPSA09999", hint: "Installed on 10.0.0.3" },
    ]);
  });
});
