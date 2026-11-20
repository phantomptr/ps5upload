import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// A static render reads the store's live state, not its initial one.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

import { BackupLayoutChoice, exportDests, layoutExample, layoutSupported } from "./BackupLayoutChoice";
import ExportDestDialog from "./ExportDestDialog";
import AutoBackupCard from "./AutoBackupCard";
import { useAutoSaveBackupStore } from "../../state/autoSaveBackup";

const noop = () => {};

describe("folder layout (#886)", () => {
  afterEach(() => useAutoSaveBackupStore.setState({ settings: null, status: null, error: null }));

  it("is offered only by an engine that can arrange backups by user", () => {
    expect(layoutSupported({ layout: "by_game" })).toBe(true);
    expect(layoutSupported({})).toBe(false);
    expect(layoutSupported(null)).toBe(false);
    const base = { enabled: false, dir: "/data/save-backups", keep: 5, hosts: [] };
    useAutoSaveBackupStore.setState({ settings: { ...base, layout: "by_game" } });
    expect(renderToStaticMarkup(<AutoBackupCard />)).toContain("auto-save-backup-layout");
    useAutoSaveBackupStore.setState({ settings: base });
    expect(renderToStaticMarkup(<AutoBackupCard />)).not.toContain("auto-save-backup-layout");
  });

  it("shows where a backup goes in each layout", () => {
    expect(layoutExample("by_game", false)).toBe("PPSA01234/PPSA01234_user1a2b3c4d_2026-11-17_101500.zip");
    expect(layoutExample("by_user", false)).toBe(
      "Alex_1a2b3c4d/Encrypted/PS5/PPSA01234/PPSA01234_user1a2b3c4d_2026-11-17_101500.zip",
    );
    expect(layoutExample("by_user", true)).toContain("/PPSA01234 - Game Name/");
  });

  it("keeps the by-user extras hidden by game, and warns about decrypted copies by user", () => {
    const byGame = renderToStaticMarkup(
      <BackupLayoutChoice testId="t" settings={{ layout: "by_game" }} status={null} onChange={noop} />,
    );
    expect(byGame).toMatch(/<option value="by_game" selected="">By game<\/option>/);
    expect(byGame).not.toContain("t-decrypted");
    expect(byGame).not.toContain("t-game-names");
    const byUser = renderToStaticMarkup(
      <BackupLayoutChoice
        testId="t"
        settings={{ layout: "by_user", game_names: true, decrypted_copy: true }}
        status={{ at: 1_700_000_000_000, made: 2, failed: 1, note: "A save is open in Save Data." }}
        onChange={noop}
      />,
    );
    expect(byUser).toMatch(/<option value="by_user" selected="">By user<\/option>/);
    expect(byUser).toContain("Add the game&#x27;s name to its folder");
    expect(byUser).toContain("Also save a decrypted copy of each backup");
    expect(byUser).toContain("anyone who has the file can load the save on another PS5");
    expect(byUser).toContain("2 made, 1 failed.");
    expect(byUser).toContain("A save is open in Save Data.");
    // An engine setting it leaves out reads as by game.
    expect(renderToStaticMarkup(<BackupLayoutChoice testId="t" settings={{}} status={null} onChange={noop} />)).toMatch(
      /<option value="by_game" selected="">/,
    );
  });

  it("offers the Decrypted folders of the places arranged by user, then this device", () => {
    const local = { dir: "D:/PS5 Game Saves", layout: "by_user" as const };
    const server = { target: "remote://nas/saves", layout: "by_user" as const };
    expect(exportDests(local, server)).toEqual(["local", "server", "device"]);
    expect(exportDests({ ...local, layout: "by_game" }, server)).toEqual(["server", "device"]);
    expect(exportDests({ ...local, dir: " " }, { ...server, target: "" })).toEqual([]);
    // Nothing by user: no choice to make, the file is saved as before.
    expect(exportDests({ dir: "/b" }, { target: "remote://x/y" })).toEqual([]);
    expect(exportDests(null, null)).toEqual([]);
  });

  it("starts on the first place's Decrypted folder and still offers the others", () => {
    const html = renderToStaticMarkup(
      <ExportDestDialog game="Astro Bot" who="Alex" dests={["server", "device"]} onPick={noop} onClose={noop} />,
    );
    expect(html).toContain("On the network share");
    expect(html).toContain("On this device");
    expect(html).toContain("In Alex&#x27;s Decrypted folder");
    expect(html).toMatch(/data-testid="save-export-server"[^>]*checked=""/);
    expect(html).not.toMatch(/data-testid="save-export-device"[^>]*checked=""/);
  });
});
