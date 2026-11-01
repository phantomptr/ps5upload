import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test:19113" }));

import { GameIcon, gameIconCandidates, iconInitials } from "./GameIcon";

describe("GameIcon", () => {
  it("shows a suffixed save folder's icon by its title id", () => {
    const out = renderToStaticMarkup(<GameIcon host="10.0.0.2" titleId="PPSA17221.bak" />);
    expect(out).toContain("title_id=PPSA17221&amp;size=128");
    expect(out).not.toContain("PPSA17221.bak");
  });

  it("asks the engine for a scaled cover sized to the icon (#450)", () => {
    expect(renderToStaticMarkup(<GameIcon host="10.0.0.2" titleId="PPSA17221" size={112} />)).toContain(
      "size=256",
    );
    expect(renderToStaticMarkup(<GameIcon host="10.0.0.2" titleId="PPSA17221" fill />)).toContain("size=384");
  });

  it("asks the console nothing for a name that isn't a title id", () => {
    const out = renderToStaticMarkup(<GameIcon host="10.0.0.2" titleId="sce_sdmemory" />);
    expect(out).not.toContain("app-icon");
  });

  it("tries the source's own icon, then the console's, then its game folder, then the online cover", () => {
    const urls = gameIconCandidates({
      host: "10.0.0.2",
      titleId: "PPSA00001",
      gamePath: "/data/games/PPSA00001",
      sourceSrc: "data:image/png;base64,AA==",
      fallbackSrc: "https://cdn.test/cover.png",
      px: 128,
    }).map((c) => c.url);
    expect(urls).toHaveLength(4);
    expect(urls[0]).toBe("data:image/png;base64,AA==");
    expect(urls[1]).toContain("app-icon");
    expect(urls[2]).toContain(encodeURIComponent("/data/games/PPSA00001"));
    expect(urls[3]).toBe("https://cdn.test/cover.png");
  });

  it("leaves out what it lacks, and the console's sources without a console", () => {
    expect(
      gameIconCandidates({ host: "", titleId: "PPSA00001", sourceSrc: "data:x", fallbackSrc: "https://c", px: 128 }).map(
        (c) => c.url,
      ),
    ).toEqual(["data:x", "https://c"]);
    expect(gameIconCandidates({ host: "10.0.0.2", px: 128 })).toEqual([]);
  });

  it("shows the source's icon first", () => {
    const out = renderToStaticMarkup(
      <GameIcon host="10.0.0.2" titleId="PPSA00001" sourceSrc="data:image/png;base64,AA==" />,
    );
    expect(out).toContain('src="data:image/png;base64,AA=="');
  });
});

describe("GameIcon without art yet", () => {
  it("keeps the image hidden until it loads, so a failing cover never shows a broken image", () => {
    const out = renderToStaticMarkup(<GameIcon host="10.0.0.2" titleId="PPSA00001" />);
    expect(out).toMatch(/<img[^>]*opacity-0/);
  });

  it("shows the game's initials in place of the glyph when the name is known", () => {
    const name = "Quiet Ocean";
    const out = renderToStaticMarkup(<GameIcon host="" titleId="PPSA00001" label={name} />);
    expect(out).toContain(">QO<");
    expect(out).not.toContain("<img");
    expect(iconInitials("  saros ")).toBe("S");
    expect(iconInitials(null)).toBe("");
  });
});
