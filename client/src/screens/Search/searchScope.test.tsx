import { renderToStaticMarkup as ssr } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { FOLDER_SCOPE, folderScopeOf, scopeRootsFor, SearchPanel } from "./SearchPanel";

// #819: search the folder Files has open instead of every drive.
describe("search scope", () => {
  const games = [{ titleId: "PPSA01234", source: "/mnt/ext0/games/PPSA01234" }, { titleId: "PPSA05678", source: "" }];

  it("walks only the open folder when This folder is chosen", () => {
    expect(scopeRootsFor(FOLDER_SCOPE, "/mnt/usb0/pkgs", games)).toEqual(["/mnt/usb0/pkgs"]);
  });

  it("walks every drive for All content, and a game's folder for a game", () => {
    expect(scopeRootsFor("", "/mnt/usb0/pkgs", games)).toBeUndefined();
    expect(scopeRootsFor("PPSA01234", "/mnt/usb0/pkgs", games)).toEqual(["/mnt/ext0/games/PPSA01234"]);
    expect(scopeRootsFor("PPSA05678", null, games)).toEqual(["/user/app/PPSA05678"]);
  });

  it("never widens This folder to a guess when Files has no folder open", () => {
    expect(scopeRootsFor(FOLDER_SCOPE, null, games)).toBeUndefined();
  });

  it("offers no folder at the top of Files, and drops a trailing slash", () => {
    expect(folderScopeOf("/")).toBeNull();
    expect(folderScopeOf("")).toBeNull();
    expect(folderScopeOf(null)).toBeNull();
    expect(folderScopeOf("/data/pkgs/")).toBe("/data/pkgs");
  });

  it("shows This folder in the scope list, picked when asked for", () => {
    const html = ssr(
      <SearchPanel request={{ n: 1, pattern: "", inFolder: true }} onOpenHit={() => {}} folder="/data/pkgs" />,
    );
    expect(html).toContain("This folder: /data/pkgs");
    expect(html).toMatch(new RegExp(`<option value="${FOLDER_SCOPE}" selected=""`));
  });

  it("has no folder choice when Files has none open", () => {
    const html = ssr(<SearchPanel onOpenHit={() => {}} folder={null} />);
    expect(html).not.toContain("This folder");
  });
});
