import { beforeEach, describe, expect, it } from "vitest";

import { pickLocalPath, useLocalPickerStore } from "./localPicker";

beforeEach(() => {
  useLocalPickerStore.setState({ pending: null });
});

describe("localPicker store", () => {
  it("opens a request and resolves it with the settled path", async () => {
    const p = useLocalPickerStore.getState().open({ mode: "folder" });
    const pending = useLocalPickerStore.getState().pending;
    expect(pending?.mode).toBe("folder");

    useLocalPickerStore.getState().settle("/storage/emulated/0/Download");
    await expect(p).resolves.toBe("/storage/emulated/0/Download");
    // Cleared after settling.
    expect(useLocalPickerStore.getState().pending).toBeNull();
  });

  it("resolves null when cancelled", async () => {
    const p = useLocalPickerStore.getState().open({ mode: "file" });
    useLocalPickerStore.getState().settle(null);
    await expect(p).resolves.toBeNull();
  });

  it("cancels a prior request when a new one opens (only one at a time)", async () => {
    const first = useLocalPickerStore.getState().open({ mode: "folder" });
    const second = useLocalPickerStore.getState().open({ mode: "file" });
    // The first promise resolves null immediately; the second is pending.
    await expect(first).resolves.toBeNull();
    expect(useLocalPickerStore.getState().pending?.mode).toBe("file");

    useLocalPickerStore.getState().settle("/storage/emulated/0/x.pkg");
    await expect(second).resolves.toBe("/storage/emulated/0/x.pkg");
  });

  it("pickLocalPath() is a thin wrapper over open()", async () => {
    const p = pickLocalPath({ mode: "file", title: "Pick a .pkg" });
    expect(useLocalPickerStore.getState().pending?.title).toBe("Pick a .pkg");
    useLocalPickerStore.getState().settle("/storage/emulated/0/game.pkg");
    await expect(p).resolves.toBe("/storage/emulated/0/game.pkg");
  });
});
