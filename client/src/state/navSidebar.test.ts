import { beforeEach, describe, expect, it, vi } from "vitest";
import { installWindowStub } from "../test-utils/storage";

const HIDDEN_KEY = "ps5upload.desktop-sidebar.hidden.v1";
const CLOSED_KEY = "ps5upload.desktop-sidebar.closed-sections.v1";
const PINNED_KEY = "ps5upload.desktop-sidebar.pinned.v1";

/** The store reads storage at module load: seed, then re-import. */
async function freshStore() {
  vi.resetModules();
  return (await import("./navSidebar")).useNavSidebarStore;
}

beforeEach(() => {
  installWindowStub();
});

describe("the sidebar's hidden screens", () => {
  it("starts with nothing hidden and every section open", async () => {
    const s = (await freshStore()).getState();
    expect(s.hidden).toEqual([]);
    expect(s.closedSections).toEqual([]);
  });

  it("hides and shows a screen, and remembers it across a relaunch", async () => {
    const storage = installWindowStub();
    const store = await freshStore();
    store.getState().toggleHidden("/cheats");
    expect(store.getState().hidden).toContain("/cheats");
    expect(JSON.parse(storage.getItem(HIDDEN_KEY)!)).toEqual(["/cheats"]);
    const again = await freshStore();
    expect(again.getState().hidden).toEqual(["/cheats"]);
    again.getState().toggleHidden("/cheats");
    expect(again.getState().hidden).toEqual([]);
  });

  it("collapses a section and remembers it", async () => {
    const storage = installWindowStub();
    const store = await freshStore();
    store.getState().toggleSection("nav_section_diagnostics");
    expect(store.getState().closedSections).toEqual(["nav_section_diagnostics"]);
    expect(JSON.parse(storage.getItem(CLOSED_KEY)!)).toEqual(["nav_section_diagnostics"]);
    store.getState().toggleSection("nav_section_diagnostics");
    expect(store.getState().closedSections).toEqual([]);
  });

  it("treats a damaged stored value as nothing hidden", async () => {
    installWindowStub({ [HIDDEN_KEY]: "{not json", [CLOSED_KEY]: JSON.stringify([1, "nav_section_help"]) });
    const s = (await freshStore()).getState();
    expect(s.hidden).toEqual([]);
    expect(s.closedSections).toEqual(["nav_section_help"]);
  });

  it("takes the settings-file mirror's lists at start-up", async () => {
    const store = await freshStore();
    store.getState().setAll(["/shell", 7 as unknown as string], ["nav_section_advanced"]);
    expect(store.getState().hidden).toEqual(["/shell"]);
    expect(store.getState().closedSections).toEqual(["nav_section_advanced"]);
  });
});

describe("the sidebar's pinned screens", () => {
  it("starts with nothing pinned", async () => {
    expect((await freshStore()).getState().pinned).toEqual([]);
  });

  it("pins in pin order, unpins, and remembers it across a relaunch", async () => {
    const storage = installWindowStub();
    const store = await freshStore();
    store.getState().togglePinned("/files");
    store.getState().togglePinned("/send");
    store.getState().togglePinned("/payloads");
    expect(store.getState().pinned).toEqual(["/files", "/send", "/payloads"]);
    expect(JSON.parse(storage.getItem(PINNED_KEY)!)).toEqual(["/files", "/send", "/payloads"]);
    store.getState().togglePinned("/send");
    expect(store.getState().pinned).toEqual(["/files", "/payloads"]);
    const again = await freshStore();
    expect(again.getState().pinned).toEqual(["/files", "/payloads"]);
  });

  it("moves a pin up and down, and by drag to a place, never past the ends", async () => {
    const store = await freshStore();
    for (const to of ["/files", "/send", "/workshop"]) store.getState().togglePinned(to);
    store.getState().movePinned("/workshop", -1);
    expect(store.getState().pinned).toEqual(["/files", "/workshop", "/send"]);
    store.getState().movePinned("/files", -1);
    expect(store.getState().pinned).toEqual(["/files", "/workshop", "/send"]);
    store.getState().movePinned("/send", 1);
    expect(store.getState().pinned).toEqual(["/files", "/workshop", "/send"]);
    store.getState().movePinnedTo("/send", 0);
    expect(store.getState().pinned).toEqual(["/send", "/files", "/workshop"]);
    store.getState().movePinnedTo("/send", 99);
    expect(store.getState().pinned).toEqual(["/files", "/workshop", "/send"]);
  });

  it("pins at most twelve, and only real screens other than Home and Bug report", async () => {
    const { NAV_ITEMS, MAX_PINNED } = await import("../layout/navItems");
    const store = await freshStore();
    const screens = NAV_ITEMS.map((i) => i.to);
    for (const to of screens.slice(0, MAX_PINNED + 2)) store.getState().togglePinned(to);
    expect(store.getState().pinned).toEqual(screens.slice(0, MAX_PINNED));
    // Full: unpinning still works and frees a place.
    store.getState().togglePinned(screens[0]);
    store.getState().togglePinned(screens[MAX_PINNED]);
    expect(store.getState().pinned).toHaveLength(MAX_PINNED);
    expect(store.getState().pinned[MAX_PINNED - 1]).toBe(screens[MAX_PINNED]);
    installWindowStub();
    const empty = await freshStore();
    for (const to of ["/home", "/bug-report", "/no-such-screen"]) empty.getState().togglePinned(to);
    expect(empty.getState().pinned).toEqual([]);
  });

  it("drops unknown, removed, repeated and junk ids on load", async () => {
    installWindowStub({
      [PINNED_KEY]: JSON.stringify(["/files", "/favorites-old", 4, "/files", "/home", "/send"]),
    });
    expect((await freshStore()).getState().pinned).toEqual(["/files", "/send"]);
  });

  it("keeps a pinned screen pinned when it is hidden (Pinned overrides hidden)", async () => {
    const store = await freshStore();
    store.getState().togglePinned("/cheats");
    store.getState().toggleHidden("/cheats");
    expect(store.getState().pinned).toEqual(["/cheats"]);
    expect(store.getState().hidden).toEqual(["/cheats"]);
  });

  it("takes the settings-file mirror's pins, and keeps its own when the file has none", async () => {
    const storage = installWindowStub();
    const store = await freshStore();
    store.getState().togglePinned("/files");
    // A file from before pinning: no `pinned` list.
    store.getState().setAll([], []);
    expect(store.getState().pinned).toEqual(["/files"]);
    store.getState().setAll([], [], ["/workshop", "/gone", "/files"]);
    expect(store.getState().pinned).toEqual(["/workshop", "/files"]);
    expect(JSON.parse(storage.getItem(PINNED_KEY)!)).toEqual(["/workshop", "/files"]);
  });

  it("counts only the shown pins toward twelve", async () => {
    const { NAV_ITEMS, MAX_PINNED } = await import("../layout/navItems");
    const store = await freshStore();
    const screens = NAV_ITEMS.map((i) => i.to);
    for (const to of screens.slice(0, MAX_PINNED)) store.getState().togglePinned(to);
    // One of them isn't shown now (say a beta screen with beta off): there is room for one more.
    const shown = screens.slice(1, MAX_PINNED);
    store.getState().togglePinned(screens[MAX_PINNED], shown);
    expect(store.getState().pinned).toHaveLength(MAX_PINNED + 1);
    // Now twelve shown: full again.
    store.getState().togglePinned(screens[MAX_PINNED + 1], [...shown, screens[MAX_PINNED]]);
    expect(store.getState().pinned).toHaveLength(MAX_PINNED + 1);
    // And all thirteen survive a relaunch.
    expect((await freshStore()).getState().pinned).toHaveLength(MAX_PINNED + 1);
  });

  it("steps a move over pins that aren't shown, so every press visibly moves", async () => {
    const store = await freshStore();
    for (const to of ["/files", "/cheats", "/send"]) store.getState().togglePinned(to);
    const shown = ["/files", "/send"];
    store.getState().movePinned("/send", -1, shown);
    expect(store.getState().pinned).toEqual(["/send", "/files", "/cheats"]);
    store.getState().movePinned("/send", 1, shown);
    expect(store.getState().pinned).toEqual(["/files", "/send", "/cheats"]);
    // Already last among the shown: nothing to do.
    store.getState().movePinned("/send", 1, shown);
    expect(store.getState().pinned).toEqual(["/files", "/send", "/cheats"]);
  });
});
