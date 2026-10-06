import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/ps5", () => ({
  resumeTxidLookup: vi.fn(),
  resumeTxidRemember: vi.fn(),
}));

import { resumeTxidLookup, resumeTxidRemember } from "../api/ps5";
import { fileResumeTxId } from "./fileResumeTx";

const lookup = vi.mocked(resumeTxidLookup);
const remember = vi.mocked(resumeTxidRemember);

const HOST = "192.168.1.10";
const SRC = "D:/games/EA.SPORTS.FC.27.ffpkg";
const DEST = "/data/homebrew/EA.SPORTS.FC.27.ffpkg";

describe("fileResumeTxId", () => {
  beforeEach(() => {
    lookup.mockReset();
    remember.mockReset();
  });

  it("reuses the id of an earlier attempt, so the kept partial is resumed (#401)", async () => {
    lookup.mockResolvedValue("d8f27e9d000000000000000000000000");
    const id = await fileResumeTxId(HOST, SRC, DEST, "9af0538e000000000000000000000000");
    expect(id).toBe("d8f27e9d000000000000000000000000");
    expect(remember).not.toHaveBeenCalled();
  });

  it("remembers a first attempt's id as a file upload", async () => {
    lookup.mockResolvedValue(null);
    remember.mockResolvedValue();
    const id = await fileResumeTxId(HOST, SRC, DEST, "aa");
    expect(id).toBe("aa");
    expect(remember).toHaveBeenCalledWith(HOST, SRC, DEST, "aa", "file");
  });

  it("falls back to the fresh id when there is no store (the browser build)", async () => {
    lookup.mockRejectedValue(new Error("unknown command"));
    remember.mockRejectedValue(new Error("unknown command"));
    await expect(fileResumeTxId(HOST, SRC, DEST, "bb")).resolves.toBe("bb");
  });
});
