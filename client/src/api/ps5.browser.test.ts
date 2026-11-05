import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import { resumeTxidForget, resumeTxidLookup, resumeTxidRemember } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => false }));

beforeEach(() => vi.mocked(invoke).mockReset());

// The record of a transfer's id lives in the desktop app: a browser asks for nothing, instead of
// filling its log with "not supported" for every upload.
it("the web UI has no resume record, and does not ask for one", async () => {
  await expect(resumeTxidLookup("h", "/a", "/b")).resolves.toBeNull();
  await expect(resumeTxidRemember("h", "/a", "/b", "00", "dir")).resolves.toBeUndefined();
  await expect(resumeTxidForget("h", "/a", "/b")).resolves.toBeUndefined();
  expect(invoke).not.toHaveBeenCalled();
});
