import { describe, expect, it } from "vitest";

import { useFsClipboardStore, type ClipboardItem } from "./fsClipboard";
import { useConnectionStore } from "./connection";

const item = (path: string): ClipboardItem => ({
  path,
  name: path.split("/").pop() ?? path,
  size: 0,
});

describe("fsClipboard — per-console stash on switchToHost", () => {
  const clip = () => useFsClipboardStore.getState();
  // switchToHost reads the OLD host from the connection store (it runs before
  // connection.setHost), so set the connection host first to mirror the roster.
  const setActiveHost = (h: string) => useConnectionStore.setState({ host: h });

  it("keeps each console's cut/copy across a round-trip switch", () => {
    // Console A: cut a file.
    setActiveHost("172.16.0.10");
    clip().set("cut", [item("/data/a.bin")], "A");

    // Switch to B — A's clipboard is stashed, B starts empty.
    clip().switchToHost("172.16.0.20");
    expect(clip().items).toHaveLength(0);
    expect(clip().op).toBeNull();

    // B (connection follows), copy something there.
    setActiveHost("172.16.0.20");
    clip().set("copy", [item("/data/b.bin")], "B");

    // Back to A → A's cut restored.
    clip().switchToHost("172.16.0.10");
    expect(clip().op).toBe("cut");
    expect(clip().items[0]?.path).toBe("/data/a.bin");

    // A → B → B's copy restored (not lost).
    setActiveHost("172.16.0.10");
    clip().switchToHost("172.16.0.20");
    expect(clip().op).toBe("copy");
    expect(clip().items[0]?.path).toBe("/data/b.bin");
  });

  it("is a no-op for the same console (port-stripped)", () => {
    setActiveHost("172.16.9.9");
    clip().set("cut", [item("/keep.bin")], "X");
    clip().switchToHost("172.16.9.9:9114");
    expect(clip().items[0]?.path).toBe("/keep.bin");
  });
});
