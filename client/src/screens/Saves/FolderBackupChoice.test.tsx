import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import {
  FolderBackupChoice,
  foldersSupported,
  stampDate,
  withFolder,
} from "./FolderBackupChoice";

const GRIM =
  "/data/prospero-win/prefixes/grim-dawn/drive_c/users/prospero/Documents/My Games/Grim Dawn";

describe("console folders backed up with the saves (#899)", () => {
  it("is offered only by an engine that keeps the setting", () => {
    expect(foldersSupported({ folders: [] })).toBe(true);
    expect(foldersSupported({})).toBe(false);
    expect(foldersSupported(null)).toBe(false);
  });

  it("adds a folder once, as the engine keeps it", () => {
    expect(withFolder([], `${GRIM}/`)).toEqual([GRIM]);
    expect(withFolder([GRIM], GRIM)).toEqual([GRIM]);
    expect(withFolder([GRIM], GRIM.toUpperCase())).toEqual([GRIM]);
    expect(withFolder([GRIM], "/data/retro")).toEqual([GRIM, "/data/retro"]);
  });

  it("reads a backup's time from its name", () => {
    expect(stampDate("2026-11-20_031200")?.toISOString()).toBe(
      "2026-11-20T03:12:00.000Z",
    );
    expect(stampDate("2026-11-20_031200_beforerestore")?.getUTCHours()).toBe(
      3,
    );
    expect(stampDate("nope")).toBeNull();
  });

  it("lists the chosen folders with a way to stop backing each up", () => {
    const html = renderToStaticMarkup(
      <FolderBackupChoice
        testId="f"
        folders={[GRIM]}
        onChange={() => {}}
        from="share"
        driveCopies
      />,
    );
    expect(html).toContain("Also back up these folders on the PS5");
    expect(html).toContain(GRIM);
    expect(html).toContain('data-testid="f-add"');
    expect(html).toContain("Folder backups");
  });
});
