import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { scanLibrary } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
const call = vi.mocked(invoke);
beforeEach(() => { call.mockReset(); });

it("finds compressed images, case insensitively, alongside raw images", async () => {
  call.mockImplementation(async (cmd, args) => {
    if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
    if (cmd === "ps5_list_dir" && (args as { path: string }).path === "/data")
      return { entries: ["a.exfat", "b.FFPFSC", "c.ffpkg", "d.ffpfs", "notes.txt"].map((name) => ({ name, kind: "file", size: 123 })) };
    throw new Error(`unexpected ${cmd}`);
  });
  const entries = await scanLibrary("192.0.2.8");
  expect(entries.map((e) => e.imageFormat)).toEqual(["exfat", "ffpfsc", "ffpkg", "ffpfs"]);
});

it("keeps images beyond the first directory page", async () => {
  call.mockImplementation(async (cmd, args) => {
    if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
    if (cmd === "ps5_list_dir") {
      const { offset } = args as { offset: number };
      return offset === 0
        ? { entries: [{ name: "notes.txt", kind: "file" }], truncated: true }
        : { entries: [{ name: "last.ffpfsc", kind: "file", size: 20 }], truncated: false };
    }
    throw new Error(`unexpected ${cmd}`);
  });
  expect((await scanLibrary("192.0.2.8")).map((e) => e.name)).toEqual(["last.ffpfsc"]);
  expect(call).toHaveBeenCalledWith("ps5_list_dir", expect.objectContaining({ offset: 1 }));
});
