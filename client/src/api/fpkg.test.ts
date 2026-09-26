import { describe, expect, it, vi } from "vitest";

vi.mock("../lib/invokeLogged", () => ({ invoke: vi.fn(async () => ({ ok: true })) }));
import { invoke } from "../lib/invokeLogged";
import { fpkg } from "./fpkg";

describe("fpkg.deletePackage", () => {
  it("asks the engine to delete exactly that package", async () => {
    await fpkg.deletePackage("/out/PPSA01234.pkg");
    expect(invoke).toHaveBeenCalledWith("fpkg_delete", { path: "/out/PPSA01234.pkg" });
  });
});

describe("fpkg.estimate", () => {
  it("asks for the estimates on their own, apart from the check", async () => {
    await fpkg.estimate("/games/a");
    expect(invoke).toHaveBeenCalledWith("fpkg_estimate", { source: "/games/a" });
  });
});
