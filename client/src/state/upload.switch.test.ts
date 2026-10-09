import { beforeEach, describe, expect, it, vi } from "vitest";

// A game folder picked on console A whose inspection finishes after the user switched to B
// belongs to A's draft: back on A, the game info is there; B's draft is untouched.
let finishInspect: (v: unknown) => void = () => {};
vi.mock("../api/ps5", async (orig) => ({
  ...(await orig<typeof import("../api/ps5")>()),
  inspectFolder: vi.fn(() => new Promise((r) => (finishInspect = r))),
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

import { useConnectionStore } from "./connection";
import { useUploadStore } from "./upload";

const A = "192.168.0.5";
const B = "192.168.0.6";
const game = { meta_source: "param.json", title: "Test Game", title_id: "PPSA01234", total_size: 1, file_count: 1 };

const switchTo = (host: string) => {
  useUploadStore.getState().switchToHost(host); // runs before setHost, as the roster does
  useConnectionStore.getState().setHost(host);
};

describe("an Upload folder pick across a console switch", () => {
  beforeEach(() => {
    useConnectionStore.getState().setHost(A);
    useUploadStore.setState({ source: null, detecting: false, detectError: null });
  });

  it("lands in the draft of the console it was picked on", async () => {
    const pick = useUploadStore.getState().pickFolder("/games/Test");
    switchTo(B);
    finishInspect({ result: game, wrapped_hint: null });
    await pick;
    expect(useUploadStore.getState().source).toBeNull();

    switchTo(A);
    const s = useUploadStore.getState().source;
    expect(s?.kind).toBe("game-folder");
    expect(s?.meta).toMatchObject({ title_id: "PPSA01234" });
  });

  it("is dropped if A's draft moved on to another source meanwhile", async () => {
    const pick = useUploadStore.getState().pickFolder("/games/Test");
    switchTo(B);
    switchTo(A);
    useUploadStore.setState({ source: { kind: "folder", path: "/games/Other", meta: null, wrappedHint: null, zipInfo: null } });
    finishInspect({ result: game, wrapped_hint: null });
    await pick;
    expect(useUploadStore.getState().source?.path).toBe("/games/Other");
  });
});
