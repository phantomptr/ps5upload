import { describe, expect, it } from "vitest";
import { useUploadStore } from "../state/upload";
import { imageDestFor } from "./imageDest";

describe("imageDestFor", () => {
  it("is the game's own destination when it has one", () => {
    const own = { volume: "/mnt/ext0", subpath: "homebrew" };
    expect(imageDestFor(own, "192.0.2.1")).toBe(own);
  });

  it("is what is set for the console now, for a game with none", () => {
    useUploadStore.setState({ destinationSubpath: "games" });
    expect(imageDestFor(null, "192.0.2.1").subpath).toBe("games");
    expect(imageDestFor(undefined, "192.0.2.1").subpath).toBe("games");
  });
});
