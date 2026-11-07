import { describe, expect, it } from "vitest";

import {
  fittedName,
  formatOfName,
  hiddenName,
  nameFormatProblem,
  nameOverLimit,
  renderName,
  shownName,
  suggestedName,
} from "./nameFormats";

const v = { title: "Game: The Sequel", titleid: "PPSA00000", version: "01.004.000", format: "FFPFSC", name: "old" };

describe("name formats", () => {
  it("fills fields, with [word] as text when it is no field", () => {
    expect(renderName("{title} [{titleid}]", v)).toBe("Game - The Sequel [PPSA00000]");
    expect(renderName("[titleid] [USA]", v)).toBe("PPSA00000 [USA]");
    expect(renderName("{{x}} {id}", v)).toBe("{x} PPSA00000");
  });

  it("applies modifiers in order", () => {
    expect(renderName("{title:upper:4}", v)).toBe("GAME");
    expect(renderName("v{version:short}", v)).toBe("v1.4");
    expect(renderName("{title:under}", { title: "A B" })).toBe("A_B");
  });

  it("leaves out an optional part, and gives up on a needed value", () => {
    expect(renderName("{id}<[v{contentid}]>", v)).toBe("PPSA00000");
    expect(renderName("{id} {contentid}", v)).toBeNull();
  });

  it("names what is wrong with a format", () => {
    expect(nameFormatProblem("{nope}")).toContain("unknown field");
    expect(nameFormatProblem("{title:nope}")).toContain("unknown modifier");
    expect(nameFormatProblem("{title")).toContain("isn't closed");
    expect(nameFormatProblem("<{title}")).toContain("isn't closed");
    expect(nameFormatProblem("")).toContain("empty");
    expect(nameFormatProblem("{title} [USA]")).toBeNull();
  });

  it("doesn't hide a game whose title starts with a dot", () => {
    expect(suggestedName("{title}", { title: ".hack//G.U." }, "a.exfat", false)).toBe("hackG.U..exfat");
    expect(suggestedName("{title}", { title: ".hack" }, ".a.exfat", false)).toBe(".hack.exfat");
  });

  it("keeps a hidden name hidden and a file's extension", () => {
    expect(suggestedName("{id}", v, ".old.exfat", false)).toBe(".PPSA00000.exfat");
    expect(suggestedName("{id}", v, "my.folder", true)).toBe("PPSA00000");
  });

  it("flags and cuts a name ShadowMount+ wouldn't mount, by bytes", () => {
    expect(nameOverLimit(`${"a".repeat(58)}.ffpfsc`)).toBe(0);
    expect(nameOverLimit(`${"a".repeat(59)}.ffpfsc`)).toBe(1);
    expect(nameOverLimit(`${"a".repeat(64)}.exfat`)).toBe(1);
    expect(nameOverLimit(`${"é".repeat(32)}.exfat`)).toBe(1);
    expect(nameOverLimit(`${"a".repeat(300)}.pkg`)).toBe(0);
    const long = { title: "One Two Three Four Five Six Seven Eight Nine Ten Eleven", titleid: "PPSA00000" };
    const cut = fittedName("{title} [{titleid}]", long, "x.ffpfsc", false);
    expect(cut).toBe("One Two Three Four Five Six Seven Eight Nine [PPSA00000].ffpfsc");
    expect(nameOverLimit(cut ?? "")).toBe(0);
    expect(fittedName("{title}", { title: "Short" }, "x.ffpfsc", false)).toBeNull();
    const oneWord = fittedName("{title}", { title: "é".repeat(40) }, "x.ffpfsc", false) ?? "";
    expect(nameOverLimit(oneWord)).toBe(0);
    expect(oneWord.startsWith("é")).toBe(true);
  });

  it("hides and shows", () => {
    expect(hiddenName("a.exfat")).toBe(".a.exfat");
    expect(hiddenName(".a.exfat")).toBe(".a.exfat");
    expect(shownName(".a.exfat")).toBe("a.exfat");
    expect(formatOfName("a.FFPFSC", false)).toBe("FFPFSC");
    expect(formatOfName("a", true)).toBeNull();
  });
});
