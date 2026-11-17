import { describe, expect, it } from "vitest";

import type { InstalledTitle } from "../../api/ps5";
import { consoleNames, saveGameName, saveGameNameNow } from "./gameNames";

const title = (titleId: string, titleName: string): InstalledTitle => ({
  titleId,
  titleName,
  origin: "pkg",
  imageBacked: false,
  source: "",
  system: false,
});

describe("Save Data game names", () => {
  const names = consoleNames([
    title("PPSA01650", "Quiet Ocean"),
    // app.db had no name: appsInstalled puts the id in its place.
    title("PPSA00001", "PPSA00001"),
  ]);

  it("uses the name the console gives the game, as Games does", () => {
    expect(saveGameName("PPSA01650", names)).toBe("Quiet Ocean");
    expect(saveGameName("ppsa01650", names, "Online name")).toBe("Quiet Ocean");
  });

  it("names a backup folder after its game", () => {
    expect(saveGameName("PPSA01650.bak", names)).toBe("Quiet Ocean");
  });

  it("falls back to the online name, then to nothing (the caller shows the id)", () => {
    expect(names.has("PPSA00001")).toBe(false);
    expect(saveGameName("PPSA00001", names, "Online name")).toBe("Online name");
    expect(saveGameName("PPSA00001", names, "  ")).toBeNull();
    expect(saveGameName("PPSA00001", consoleNames(null))).toBeNull();
  });

  it("names a notification's game like the prompt, else its title ID", async () => {
    const names = new Map([["PPSA00002", "Console name"]]);
    const online = async (id: string) => (id === "PPSA00001" ? { title: "Online name" } : null);
    expect(await saveGameNameNow("PPSA00001", names, online)).toBe("Online name");
    expect(await saveGameNameNow("PPSA00002", names, online)).toBe("Console name");
    expect(await saveGameNameNow("PPSA00003", names, online)).toBe("PPSA00003");
    const down = async () => {
      throw new Error("offline");
    };
    expect(await saveGameNameNow("PPSA00003", names, down)).toBe("PPSA00003");
  });
});
