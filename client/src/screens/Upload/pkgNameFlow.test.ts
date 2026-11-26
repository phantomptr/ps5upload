import { describe, expect, it, vi } from "vitest";

import type { PkgNames, PkgRenamed } from "../../api/pkgName";
import { createNameFlow, saveBlocked } from "./pkgNameFlow";

const names = (over: Partial<PkgNames> = {}): PkgNames => ({
  platform: "ps5",
  content_id: "UP0000-PPSA01234_00-TESTGAME00000000",
  name: "A Game-example.invalid",
  entries: [{ key: "en-US", name: "A Game-example.invalid" }],
  other_names: 0,
  refusal: null,
  max_bytes: 127,
  ...over,
});

const renamed = (name: string, changed = 1): PkgRenamed => ({
  platform: "ps5",
  previous: "A Game-example.invalid",
  name,
  changed,
  written_bytes: 2000,
});

describe("the Name on the PS5 field", () => {
  it("reads a picked package's name into the field", async () => {
    const read = vi.fn().mockResolvedValue(names());
    const flow = createNameFlow({ read, set: vi.fn() });
    const seen: boolean[] = [];
    flow.subscribe(() => seen.push(flow.get().loading));
    await flow.load("/games/a.pkg");
    expect(read).toHaveBeenCalledWith("/games/a.pkg");
    expect(flow.get()).toMatchObject({ path: "/games/a.pkg", value: "A Game-example.invalid", loading: false });
    expect(seen).toEqual([true, false]);
    // The same name is nothing to change; another one is.
    expect(saveBlocked(flow.get())).toBe("same");
    flow.setValue("   ");
    expect(saveBlocked(flow.get())).toBe("empty");
    flow.setValue(" A Game ");
    expect(saveBlocked(flow.get())).toBeNull();
    // No package: back to nothing.
    await flow.load(null);
    expect(flow.get()).toMatchObject({ path: null, names: null, value: "" });
  });

  it("changes the name, reads the package again and says it was renamed", async () => {
    const read = vi
      .fn()
      .mockResolvedValueOnce(names({ other_names: 2 }))
      .mockResolvedValueOnce(names({ name: "A Game", entries: [{ key: "en-US", name: "A Game" }] }));
    const set = vi.fn().mockResolvedValue(renamed("A Game", 3));
    const onRenamed = vi.fn();
    const flow = createNameFlow({ read, set, renamed: onRenamed });
    await flow.load("/games/a.pkg");
    flow.setValue("  A Game ");
    flow.setAllLanguages(true);
    const saving: boolean[] = [];
    flow.subscribe(() => saving.push(flow.get().saving));
    await flow.save();
    expect(set).toHaveBeenCalledWith("/games/a.pkg", "A Game", true);
    expect(saving[0]).toBe(true);
    expect(flow.get()).toMatchObject({
      saving: false,
      value: "A Game",
      allLanguages: false,
      saved: { changed: 3 },
      error: null,
    });
    expect(flow.get().names?.name).toBe("A Game");
    expect(onRenamed).toHaveBeenCalledWith("/games/a.pkg");
    expect(saveBlocked(flow.get())).toBe("same");
  });

  it("every language can be asked for with the name unchanged", async () => {
    const flow = createNameFlow({ read: vi.fn().mockResolvedValue(names({ other_names: 3 })), set: vi.fn() });
    await flow.load("/games/a.pkg");
    expect(saveBlocked(flow.get())).toBe("same");
    flow.setAllLanguages(true);
    expect(saveBlocked(flow.get())).toBeNull();
  });

  it("keeps what was typed and gives the reason when the engine refuses", async () => {
    const set = vi.fn().mockRejectedValue(new Error("a PS5 is installing from this package"));
    const onRenamed = vi.fn();
    const flow = createNameFlow({ read: vi.fn().mockResolvedValue(names()), set, renamed: onRenamed });
    await flow.load("/games/a.pkg");
    flow.setValue("A Game");
    await flow.save();
    expect(flow.get()).toMatchObject({
      saving: false,
      value: "A Game",
      saved: null,
      error: { during: "save", reason: "a PS5 is installing from this package" },
    });
    expect(onRenamed).not.toHaveBeenCalled();
    // Typing again clears the reason.
    flow.setValue("A Game 2");
    expect(flow.get().error).toBeNull();
  });

  it("never asks to rename a package that refuses, or with nothing to change", async () => {
    const set = vi.fn();
    const refused = names({ refusal: { code: "retail", reason: "this is an official package" } });
    const flow = createNameFlow({ read: vi.fn().mockResolvedValue(refused), set });
    await flow.load("/games/official.pkg");
    flow.setValue("Another");
    expect(saveBlocked(flow.get())).toBe("refused");
    await flow.save();
    expect(set).not.toHaveBeenCalled();
  });

  it("a name that could not be read leaves no field", async () => {
    const flow = createNameFlow({
      read: vi.fn().mockRejectedValue(new Error("this is not a PS4 or PS5 package")),
      set: vi.fn(),
    });
    await flow.load("/games/x.pkg");
    expect(flow.get()).toMatchObject({ names: null, loading: false, error: { during: "read" } });
    expect(saveBlocked(flow.get())).toBe("busy");
  });

  it("drops the answer for a package picked before", async () => {
    let first: (n: PkgNames) => void = () => {};
    const read = vi
      .fn()
      .mockImplementationOnce(() => new Promise<PkgNames>((resolve) => (first = resolve)))
      .mockResolvedValueOnce(names({ name: "Second" }));
    const flow = createNameFlow({ read, set: vi.fn() });
    const slow = flow.load("/games/first.pkg");
    await flow.load("/games/second.pkg");
    first(names({ name: "First" }));
    await slow;
    expect(flow.get()).toMatchObject({ path: "/games/second.pkg", value: "Second" });
  });
});
