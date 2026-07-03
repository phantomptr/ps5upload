import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { searchPS5 } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
// invokeLogged branches on isTauriEnv() to route to browserInvoke instead of
// the mocked Tauri invoke above; force the Tauri path so this mock is used.
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

const mockedInvoke = vi.mocked(invoke);

describe("searchPS5", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("paginates each scanned directory", async () => {
    mockedInvoke.mockImplementation(async (cmd, args) => {
      if (cmd === "ps5_volumes") {
        return {
          volumes: [
            {
              path: "/data",
              writable: true,
              is_placeholder: false,
            },
          ],
        };
      }
      if (cmd === "ps5_list_dir") {
        const req = args as { path: string; offset: number };
        if (req.path === "/data" && req.offset === 0) {
          return {
            truncated: true,
            entries: Array.from({ length: 256 }, (_, i) => ({
              name: `a${i}.bin`,
              kind: "file",
              size: 1,
            })),
          };
        }
        if (req.path === "/data" && req.offset === 256) {
          return {
            truncated: false,
            entries: [{ name: "target.pkg", kind: "file", size: 2 }],
          };
        }
      }
      throw new Error(`unexpected invoke: ${cmd}`);
    });

    const result = await searchPS5("192.168.1.2:9113", "*.pkg", 0);

    expect(result.hits).toEqual([
      {
        path: "/data/target.pkg",
        name: "target.pkg",
        size: 2,
        kind: "file",
      },
    ]);
    expect(result.scanned).toBe(257);
    expect(result.truncated).toBe(false);
    expect(mockedInvoke).toHaveBeenCalledWith("ps5_list_dir", {
      addr: "192.168.1.2:9114",
      path: "/data",
      offset: 256,
      limit: 256,
    });
  });
});
