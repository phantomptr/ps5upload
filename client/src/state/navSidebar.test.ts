import { beforeEach, describe, expect, it, vi } from "vitest";

const HIDDEN_KEY = "ps5upload.desktop-sidebar.hidden.v1";
const CLOSED_KEY = "ps5upload.desktop-sidebar.closed-sections.v1";

// Plain-node environment: stand up localStorage by hand (see navItems/safeStorage).
function installStorage(seed: Record<string, string> = {}) {
  const map = new Map(Object.entries(seed));
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
      removeItem: (k: string) => void map.delete(k),
      clear: () => map.clear(),
    },
  };
  return map;
}

/** The store reads storage at module load: seed, then re-import. */
async function freshStore() {
  vi.resetModules();
  return (await import("./navSidebar")).useNavSidebarStore;
}

beforeEach(() => {
  installStorage();
});

describe("the sidebar's hidden screens", () => {
  it("starts with nothing hidden and every section open", async () => {
    const s = (await freshStore()).getState();
    expect(s.hidden).toEqual([]);
    expect(s.closedSections).toEqual([]);
  });

  it("hides and shows a screen, and remembers it across a relaunch", async () => {
    const storage = installStorage();
    const store = await freshStore();
    store.getState().toggleHidden("/cheats");
    expect(store.getState().hidden).toContain("/cheats");
    expect(JSON.parse(storage.get(HIDDEN_KEY)!)).toEqual(["/cheats"]);
    const again = await freshStore();
    expect(again.getState().hidden).toEqual(["/cheats"]);
    again.getState().toggleHidden("/cheats");
    expect(again.getState().hidden).toEqual([]);
  });

  it("collapses a section and remembers it", async () => {
    const storage = installStorage();
    const store = await freshStore();
    store.getState().toggleSection("nav_section_diagnostics");
    expect(store.getState().closedSections).toEqual(["nav_section_diagnostics"]);
    expect(JSON.parse(storage.get(CLOSED_KEY)!)).toEqual(["nav_section_diagnostics"]);
    store.getState().toggleSection("nav_section_diagnostics");
    expect(store.getState().closedSections).toEqual([]);
  });

  it("treats a damaged stored value as nothing hidden", async () => {
    installStorage({ [HIDDEN_KEY]: "{not json", [CLOSED_KEY]: JSON.stringify([1, "nav_section_help"]) });
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
